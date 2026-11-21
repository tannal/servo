/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use crate::dom::bindings::str::DOMString;

#[expect(non_snake_case)]
pub(crate) fn Product() -> DOMString {
    DOMString::from_static("Gecko")
}

#[expect(non_snake_case)]
pub(crate) fn ProductSub() -> DOMString {
    DOMString::from_static("20100101")
}

#[expect(non_snake_case)]
pub(crate) fn Vendor() -> DOMString {
    DOMString::new()
}

#[expect(non_snake_case)]
pub(crate) fn VendorSub() -> DOMString {
    DOMString::new()
}

#[expect(non_snake_case)]
pub(crate) fn TaintEnabled() -> bool {
    false
}

#[expect(non_snake_case)]
pub(crate) fn AppName() -> DOMString {
    DOMString::from_static("Netscape") // Like Gecko/Webkit
}

#[expect(non_snake_case)]
pub(crate) fn AppCodeName() -> DOMString {
    DOMString::from_static("Mozilla")
}

#[expect(non_snake_case)]
#[cfg(target_os = "windows")]
pub(crate) fn Platform() -> DOMString {
    DOMString::from_static("Win32")
}

#[expect(non_snake_case)]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) fn Platform() -> DOMString {
    DOMString::from_static("Linux x86_64")
}

#[expect(non_snake_case)]
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub(crate) fn Platform() -> DOMString {
    DOMString::from_static("Linux aarch64")
}

#[expect(non_snake_case)]
#[cfg(any(
    target_os = "android",
    target_os = "freebsd",
    all(
        target_os = "linux",
        not(any(target_arch = "x86_64", target_arch = "aarch64"))
    )
))]
pub(crate) fn Platform() -> DOMString {
    DOMString::from_static("Linux")
}

#[expect(non_snake_case)]
#[cfg(target_os = "macos")]
pub(crate) fn Platform() -> DOMString {
    DOMString::from_static("MacIntel")
}

#[expect(non_snake_case)]
#[cfg(target_os = "ios")]
pub(crate) fn Platform() -> DOMString {
    DOMString::from_static("iPhone")
}

#[expect(non_snake_case)]
pub(crate) fn UserAgent(user_agent: &str) -> DOMString {
    DOMString::from(user_agent)
}

#[expect(non_snake_case)]
#[cfg(target_os = "windows")]
pub(crate) fn AppVersion() -> DOMString {
    DOMString::from_static("5.0 (Windows)")
}

#[expect(non_snake_case)]
#[cfg(target_os = "macos")]
pub(crate) fn AppVersion() -> DOMString {
    DOMString::from_static("5.0 (Macintosh)")
}

#[expect(non_snake_case)]
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn AppVersion() -> DOMString {
    DOMString::from_static("5.0 (X11)")
}

#[expect(non_snake_case)]
pub(crate) fn Language() -> DOMString {
    DOMString::from(net_traits::get_current_locale().0.clone())
}
