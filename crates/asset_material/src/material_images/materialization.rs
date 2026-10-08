use super::decode::{CubemapFaces, DecodedMips, MipStorage};
use super::sampler_from_iw4;
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::Image;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static RESIDENT_PAYLOAD_BYTES: AtomicU64 = AtomicU64::new(0);
pub(super) fn resident_payload_bytes() -> u64 {
    RESIDENT_PAYLOAD_BYTES.load(Ordering::Relaxed)
}

enum PayloadData {
    Mips(std::sync::Mutex<PreparedMip>),
    Cubemap { size: u32, faces: Box<CubemapFaces> },
}

struct PreparedMip {
    mips: DecodedMips,
    images: Vec<(WrapRecipe, Arc<Image>)>,
    /// What this payload is charged against RESIDENT_PAYLOAD_BYTES. On iOS the
    /// first wrap changes the byte count (texture cap, BC kept or expanded to
    /// RGBA8), so the charge is moved to the wrapped size then and released as
    /// is on drop, rather than subtracting a different size later.
    accounted_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WrapRecipe {
    pub(super) sampler_state: u8,
    pub(super) is_normal: bool,
    pub(super) alpha_test_color: bool,
    pub(super) force_linear: bool,
    pub(super) use_srgb_reads: bool,
}

impl WrapRecipe {
    pub(super) fn new(
        (sampler_state, is_normal, alpha_test_color, force_linear): (u8, bool, bool, bool),
        use_srgb_reads: bool,
    ) -> Self {
        Self {
            sampler_state,
            is_normal,
            alpha_test_color,
            force_linear,
            use_srgb_reads,
        }
    }

    fn linear(self) -> bool {
        self.is_normal || !self.use_srgb_reads || self.force_linear
    }
}

pub(super) struct PreparedPayload {
    data: PayloadData,
}

impl PreparedPayload {
    pub(super) fn cubemap(size: u32, faces: CubemapFaces) -> Self {
        Self {
            data: PayloadData::Cubemap {
                size,
                faces: Box::new(faces),
            },
        }
    }
    pub(super) fn mips(mips: DecodedMips) -> Self {
        Self {
            data: PayloadData::Mips(std::sync::Mutex::new(PreparedMip {
                accounted_bytes: mips.payload().len() as u64,
                mips,
                images: Vec::new(),
            })),
        }
    }

    pub(super) fn bytes(&self) -> u64 {
        match &self.data {
            PayloadData::Mips(state) => {
                let state = state.lock().unwrap_or_else(|poison| poison.into_inner());
                state
                    .images
                    .first()
                    .map_or(state.mips.payload().len() as u64, |(_, image)| {
                        image_bytes(image)
                    })
            }
            PayloadData::Cubemap { faces, .. } => faces.iter().map(|face| face.len() as u64).sum(),
        }
    }

    /// A mip payload nobody has wrapped yet: its bytes are still the decode,
    /// not what a kept image would hold.
    pub(super) fn is_unwrapped_mips(&self) -> bool {
        matches!(&self.data, PayloadData::Mips(state)
            if state.lock().unwrap_or_else(|poison| poison.into_inner()).images.is_empty())
    }

    #[cfg(test)]
    pub(super) fn accounted_bytes(&self) -> Option<u64> {
        match &self.data {
            PayloadData::Mips(state) => Some(state.lock().unwrap().accounted_bytes),
            PayloadData::Cubemap { .. } => None,
        }
    }

    pub(super) fn owned(self) -> Arc<Self> {
        RESIDENT_PAYLOAD_BYTES.fetch_add(self.bytes(), Ordering::Relaxed);
        Arc::new(self)
    }
}

impl Drop for PreparedPayload {
    fn drop(&mut self) {
        let bytes = match &mut self.data {
            PayloadData::Mips(state) => {
                state
                    .get_mut()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .accounted_bytes
            }
            PayloadData::Cubemap { faces, .. } => faces.iter().map(|face| face.len() as u64).sum(),
        };
        if bytes > 0 {
            RESIDENT_PAYLOAD_BYTES.fetch_sub(bytes, Ordering::Relaxed);
        }
    }
}

type Prepared = (std::sync::Mutex<PreparedPayloads>, std::sync::Condvar);

#[derive(Default)]
struct PreparedPayloads {
    live: HashMap<u64, std::sync::Weak<PreparedPayload>>,
    decoding: HashSet<u64>,
}

fn prepared() -> &'static Prepared {
    static PREPARED: std::sync::OnceLock<Prepared> = std::sync::OnceLock::new();
    PREPARED.get_or_init(Default::default)
}

static SHARED_PAYLOADS: AtomicU64 = AtomicU64::new(0);
static SHARED_BYTES: AtomicU64 = AtomicU64::new(0);
static WRAPPED_BYTES: AtomicU64 = AtomicU64::new(0);
static MOVED_BYTES: AtomicU64 = AtomicU64::new(0);

pub fn shared_variant_census() -> (u64, u64) {
    (
        SHARED_PAYLOADS.load(Ordering::Relaxed),
        SHARED_BYTES.load(Ordering::Relaxed),
    )
}

pub fn shared_payload_copy_cost() -> (u64, u64) {
    (
        WRAPPED_BYTES.load(Ordering::Relaxed),
        MOVED_BYTES.load(Ordering::Relaxed),
    )
}

pub(super) fn share_or_decode<E>(
    payload: u64,
    decode: impl FnOnce() -> Result<PreparedPayload, E>,
) -> Result<(Arc<PreparedPayload>, bool), E> {
    let (lock, signal) = prepared();
    let mut state = lock.lock().unwrap_or_else(|poison| poison.into_inner());
    loop {
        if let Some(live) = state.live.get(&payload) {
            if let Some(prepared) = live.upgrade() {
                SHARED_PAYLOADS.fetch_add(1, Ordering::Relaxed);
                SHARED_BYTES.fetch_add(prepared.bytes(), Ordering::Relaxed);
                return Ok((prepared, true));
            }
            state.live.remove(&payload);
        }
        if !state.decoding.contains(&payload) {
            break;
        }
        state = signal
            .wait(state)
            .unwrap_or_else(|poison| poison.into_inner());
    }
    state.decoding.insert(payload);
    drop(state);

    // Held across the decode so that a panic in it wakes the askers waiting on
    // this payload instead of parking them for the life of the process. It is
    // dropped *after* the result is published, so a waiter that wakes finds the
    // payload rather than an empty slot it would decode again.
    let flight = Flight(payload);
    let prepared = decode().map(|prepared| {
        let prepared = prepared.owned();
        lock.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .live
            .insert(payload, Arc::downgrade(&prepared));
        (prepared, false)
    });
    drop(flight);
    prepared
}

struct Flight(u64);

impl Drop for Flight {
    fn drop(&mut self) {
        let (lock, signal) = prepared();
        lock.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .decoding
            .remove(&self.0);
        signal.notify_all();
    }
}

pub(super) fn wrap_payload(payload: &PreparedPayload, wrap: WrapRecipe) -> Arc<Image> {
    match &payload.data {
        PayloadData::Cubemap { size, faces } => Arc::new(pack_material_cubemap(
            *size,
            faces,
            wrap.use_srgb_reads && !wrap.force_linear,
        )),
        PayloadData::Mips(state) => {
            let mut state = state.lock().expect("prepared mip cache is not poisoned");
            if let Some((_, image)) = state.images.iter().find(|(recipe, _)| *recipe == wrap) {
                return Arc::clone(image);
            }
            if state.images.is_empty() {
                // Keep the residency charge while the cached image owns these bytes.
                let packed = state.mips.take_payload();
                MOVED_BYTES.fetch_add(packed.len() as u64, Ordering::Relaxed);
                let image = Arc::new(wrap_mips(&state.mips, packed, wrap));
                let bytes = image_bytes(&image);
                RESIDENT_PAYLOAD_BYTES.fetch_add(bytes, Ordering::Relaxed);
                RESIDENT_PAYLOAD_BYTES.fetch_sub(state.accounted_bytes, Ordering::Relaxed);
                state.accounted_bytes = bytes;
                state.images.push((wrap, Arc::clone(&image)));
                return image;
            }
            // The first image's bytes are already fitted for the GPU (on iOS
            // capped, and RGBA8 unless the GPU samples BC). Feeding them back
            // through wrap_mips would treat them as the original chain. A
            // different sampler or colour space needs a copy of that image, not
            // a decode: keep its format and flip only the sRGB suffix.
            let mut image = state.images[0].1.as_ref().clone();
            let bytes = image_bytes(&image);
            WRAPPED_BYTES.fetch_add(bytes, Ordering::Relaxed);
            diag::memtrack::add(diag::memtrack::Cat::ImageDupCopy, bytes);
            let format = image.texture_descriptor.format;
            image.texture_descriptor.format = if wrap.linear() {
                format.remove_srgb_suffix()
            } else {
                format.add_srgb_suffix()
            };
            image.sampler = ImageSampler::Descriptor(sampler_from_iw4(
                wrap.sampler_state,
                image.texture_descriptor.mip_level_count,
                wrap.alpha_test_color,
            ));
            Arc::new(image)
        }
    }
}

pub(super) fn texture_format(storage: MipStorage, linear: bool) -> TextureFormat {
    match (storage, linear) {
        (MipStorage::Rgba8, true) => TextureFormat::Rgba8Unorm,
        (MipStorage::Rgba8, false) => TextureFormat::Rgba8UnormSrgb,
        (MipStorage::Bc1, true) => TextureFormat::Bc1RgbaUnorm,
        (MipStorage::Bc1, false) => TextureFormat::Bc1RgbaUnormSrgb,
        (MipStorage::Bc2, true) => TextureFormat::Bc2RgbaUnorm,
        (MipStorage::Bc2, false) => TextureFormat::Bc2RgbaUnormSrgb,
        (MipStorage::Bc3, true) => TextureFormat::Bc3RgbaUnorm,
        (MipStorage::Bc3, false) => TextureFormat::Bc3RgbaUnormSrgb,
        (MipStorage::Bc5, _) => TextureFormat::Bc5RgUnorm,
    }
}

pub(super) fn wrap_mips(mips: &DecodedMips, data: Vec<u8>, wrap: WrapRecipe) -> Image {
    #[cfg(target_os = "ios")]
    let (storage, data, width, height, levels) = super::decode::ios_fit_for_gpu(mips, data);
    #[cfg(not(target_os = "ios"))]
    let (storage, width, height, levels) = {
        let layout = mips.layout();
        (layout.storage, layout.width, layout.height, layout.levels)
    };
    let format = texture_format(storage, wrap.linear());
    let mut image = Image::new_uninit(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = levels;
    image.data = Some(data);
    image.sampler = ImageSampler::Descriptor(sampler_from_iw4(
        wrap.sampler_state,
        levels,
        wrap.alpha_test_color,
    ));
    image
}

pub(super) fn pack_material_cubemap(size: u32, faces: &CubemapFaces, srgb: bool) -> Image {
    let mut pixels = Vec::with_capacity(6 * size as usize * size as usize * 4);
    for face in faces {
        pixels.extend_from_slice(face);
    }
    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size * 6,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        if srgb {
            TextureFormat::Rgba8UnormSrgb
        } else {
            TextureFormat::Rgba8Unorm
        },
        RenderAssetUsages::RENDER_WORLD,
    );
    image
        .reinterpret_stacked_2d_as_array(6)
        .expect("six equal cubemap faces");
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..Default::default()
    });
    let mut sampler = ImageSamplerDescriptor::linear();
    sampler.address_mode_u = ImageAddressMode::ClampToEdge;
    sampler.address_mode_v = ImageAddressMode::ClampToEdge;
    sampler.address_mode_w = ImageAddressMode::ClampToEdge;
    image.sampler = ImageSampler::Descriptor(sampler);
    image
}

pub(super) fn image_bytes(image: &Image) -> u64 {
    image.data.as_ref().map_or(0, Vec::len) as u64
}
