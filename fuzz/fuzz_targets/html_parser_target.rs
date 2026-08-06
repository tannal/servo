#![no_main]

use std::borrow::Cow;

use html5ever::tendril::*;
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, ParseOpts, QualName, local_name, ns, parse_document};
use libfuzzer_sys::fuzz_target;

// 1. 在 NopSink 中保存一个持久的 QualName 示例
struct NopSink {
    dummy_qual_name: QualName,
}

impl Default for NopSink {
    fn default() -> Self {
        Self {
            // 正确调用 QualName::new 构造函数
            dummy_qual_name: QualName::new(None, ns!(html), local_name!("div")),
        }
    }
}

impl TreeSink for NopSink {
    type Handle = usize;
    type Output = Self;
    // 关键修正：&'a QualName 本身就实现了 ElemName trait！
    type ElemName<'a> = &'a QualName;

    fn finish(self) -> Self {
        self
    }
    fn get_document(&self) -> usize {
        0
    }

    // 假装 1 是 <template> 节点的 Handle，遇到 template 时避免 panic
    fn get_template_contents(&self, _target: &usize) -> usize {
        1
    }

    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }

    // 直接返回 QualName 的引用即可
    fn elem_name(&self, _target: &usize) -> &QualName {
        &self.dummy_qual_name
    }

    // 所有创建和挂载操作均直接返回伪 Handle（0），不分配任何内存
    fn create_element(
        &self,
        _name: QualName,
        _attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> usize {
        0
    }
    fn create_comment(&self, _text: StrTendril) -> usize {
        0
    }
    fn create_pi(&self, _target: StrTendril, _value: StrTendril) -> usize {
        0
    }

    fn append_before_sibling(&self, _sibling: &usize, _new_node: NodeOrText<usize>) {}
    fn append_based_on_parent_node(
        &self,
        _element: &usize,
        _prev_element: &usize,
        _new_node: NodeOrText<usize>,
    ) {
    }
    fn append(&self, _parent: &usize, _child: NodeOrText<usize>) {}
    fn append_doctype_to_document(&self, _: StrTendril, _: StrTendril, _: StrTendril) {}
    fn add_attrs_if_missing(&self, _target: &usize, _attrs: Vec<Attribute>) {}
    fn remove_from_parent(&self, _target: &usize) {}
    fn reparent_children(&self, _node: &usize, _new_parent: &usize) {}
    fn mark_script_already_started(&self, _node: &usize) {}
    fn parse_error(&self, _msg: Cow<'static, str>) {}
    fn set_quirks_mode(&self, _mode: QuirksMode) {}
}

// 2. Fuzz Target 入口
fuzz_target!(|data: &[u8]| {
    let _ = parse_document(NopSink::default(), ParseOpts::default())
        .from_utf8()
        .one(data);
});
