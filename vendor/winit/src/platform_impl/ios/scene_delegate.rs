//! Minimal `UIWindowSceneDelegate`, so apps adopt the UIScene lifecycle.
//!
//! iOS 27 refuses to launch apps linked against the iOS 27 SDK that don't adopt the scene
//! lifecycle (`_UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption` traps when the
//! first scene is created). The app's `Info.plist` must name this class as the scene delegate:
//!
//! ```xml
//! <key>UIApplicationSceneManifest</key>
//! <dict>
//!     <key>UIApplicationSupportsMultipleScenes</key>
//!     <false/>
//!     <key>UISceneConfigurations</key>
//!     <dict>
//!         <key>UIWindowSceneSessionRoleApplication</key>
//!         <array>
//!             <dict>
//!                 <key>UISceneConfigurationName</key>
//!                 <string>Default Configuration</string>
//!                 <key>UISceneDelegateClassName</key>
//!                 <string>WinitSceneDelegate</string>
//!             </dict>
//!         </array>
//!     </dict>
//! </dict>
//! ```
//!
//! Windows created before the scene connects are parked here and attached to the scene (and
//! made key and visible) once it does; windows created afterwards are attached immediately.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{declare_class, msg_send, msg_send_id, mutability, ClassType, DeclaredClass};
use objc2_foundation::{CGRect, MainThreadMarker, NSObjectProtocol};
use objc2_ui_kit::{UIResponder, UISceneDelegate, UIWindowSceneDelegate};
use tracing::debug;

use super::window::WinitUIWindow;

thread_local! {
    /// The connected `UIWindowScene`, once UIKit has created it.
    static SCENE: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    /// Windows created before the scene connected, waiting to be attached.
    static PENDING: RefCell<Vec<Retained<WinitUIWindow>>> = const { RefCell::new(Vec::new()) };
}

declare_class!(
    pub(crate) struct WinitSceneDelegate;

    unsafe impl ClassType for WinitSceneDelegate {
        #[inherits(NSObject)]
        type Super = UIResponder;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "WinitSceneDelegate";
    }

    impl DeclaredClass for WinitSceneDelegate {}

    // UIKit asserts that the scene delegate class conforms to `UISceneDelegate`, so declare the
    // protocols as well as implementing the methods.
    unsafe impl NSObjectProtocol for WinitSceneDelegate {}

    unsafe impl UISceneDelegate for WinitSceneDelegate {}

    unsafe impl UIWindowSceneDelegate for WinitSceneDelegate {}

    unsafe impl WinitSceneDelegate {
        #[method(scene:willConnectToSession:options:)]
        fn scene_will_connect(&self, scene: &AnyObject, _session: &AnyObject, _options: &AnyObject) {
            debug!("scene connected");
            let scene = unsafe { Retained::retain(scene as *const AnyObject as *mut AnyObject) }
                .expect("scene to be non-null");
            SCENE.with(|s| *s.borrow_mut() = Some(scene.clone()));

            let pending = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
            for window in pending {
                set_scene(&window, &scene);
                window.makeKeyAndVisible();
            }
        }

        #[method(sceneDidDisconnect:)]
        fn scene_did_disconnect(&self, _scene: &AnyObject) {
            debug!("scene disconnected");
            SCENE.with(|s| *s.borrow_mut() = None);
        }
    }
);

/// Registers the class with the Objective-C runtime, so UIKit can find it by the name given in
/// `Info.plist`. Must be called before `UIApplicationMain`.
pub(crate) fn register() {
    let _ = WinitSceneDelegate::class();
}

/// Attaches `window` to the connected scene. Returns `false` (and parks the window until the
/// scene connects) if there is no scene yet.
pub(crate) fn attach_window(_mtm: MainThreadMarker, window: &Retained<WinitUIWindow>) -> bool {
    match SCENE.with(|s| s.borrow().clone()) {
        Some(scene) => {
            set_scene(window, &scene);
            true
        },
        None => {
            PENDING.with(|p| p.borrow_mut().push(window.clone()));
            false
        },
    }
}

/// Moves `window` into `scene` and makes it fill the scene.
///
/// The window's frame was chosen before it had a scene (from the screen bounds or the requested
/// `inner_size`, which is in points and can be larger than the screen). Windows without a scene
/// used to be resized to the screen by UIKit; windows in a scene keep their frame, so fit it here.
fn set_scene(window: &WinitUIWindow, scene: &AnyObject) {
    let _: () = unsafe { msg_send![window, setWindowScene: scene] };
    let space: Retained<AnyObject> = unsafe { msg_send_id![scene, coordinateSpace] };
    let bounds: CGRect = unsafe { msg_send![&space, bounds] };
    window.setFrame(bounds);
}

/// Whether `window` is still waiting for the scene to connect.
pub(crate) fn is_pending(window: &Retained<WinitUIWindow>) -> bool {
    PENDING.with(|p| p.borrow().iter().any(|w| w == window))
}
