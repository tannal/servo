/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use html5ever::{LocalName, Prefix, QualName, local_name, ns};
use js::rust::HandleObject;

use crate::dom::bindings::codegen::Bindings::ElementBinding::Element_Binding::ElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLAudioElementBinding::HTMLAudioElementMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::bindings::error::Fallible;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::document::Document;
use crate::dom::element::{CustomElementCreationMode, Element, ElementCreator};
use crate::dom::htmlmediaelement::HTMLMediaElement;
use crate::dom::node::Node;
use crate::dom::window::Window;
use crate::script_runtime::CanGc;

#[dom_struct]
pub(crate) struct HTMLAudioElement {
    htmlmediaelement: HTMLMediaElement,
}

impl HTMLAudioElement {
    fn new_inherited(
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
    ) -> HTMLAudioElement {
        HTMLAudioElement {
            htmlmediaelement: HTMLMediaElement::new_inherited(local_name, prefix, document),
        }
    }

    #[cfg_attr(crown, allow(crown::unrooted_must_root))]
    pub(crate) fn new(
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
        proto: Option<HandleObject>,
        can_gc: CanGc,
    ) -> DomRoot<HTMLAudioElement> {
        Node::reflect_node_with_proto(
            Box::new(HTMLAudioElement::new_inherited(
                local_name, prefix, document,
            )),
            document,
            proto,
            can_gc,
        )
    }
}

impl HTMLAudioElementMethods<crate::DomTypeHolder> for HTMLAudioElement {
    // https://html.spec.whatwg.org/multipage/#dom-audio
    fn Audio(
        window: &Window,
        proto: Option<HandleObject>,
        can_gc: CanGc,
        src: Option<DOMString>,
    ) -> Fallible<DomRoot<HTMLAudioElement>> {
        let element = Element::create(
            QualName::new(None, ns!(html), local_name!("audio")),
            None,
            &window.Document(),
            ElementCreator::ScriptCreated,
            CustomElementCreationMode::Synchronous,
            proto,
            can_gc,
        );

        let audio = DomRoot::downcast::<HTMLAudioElement>(element).unwrap();

        audio
            .upcast::<Element>()
            .SetAttribute(DOMString::from("preload"), DOMString::from("auto"), can_gc)
            .expect("should be infallible");
        if let Some(s) = src {
            audio
                .upcast::<Element>()
                .SetAttribute(DOMString::from("src"), s, can_gc)
                .expect("should be infallible");
        }

        Ok(audio)
    }
}
