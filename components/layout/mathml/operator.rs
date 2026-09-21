/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use app_units::Au;
use euclid::num::Zero;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::context::SharedStyleContext;
use style::dom::NodeInfo;
use style::properties::ComputedValues;
use web_atoms::{local_name, ns};

use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::flow::CollapsibleWithParentStartMargin;
use crate::flow::inline::InlineFormattingContext;
use crate::formatting_contexts::Baselines;
use crate::fragment_tree::{BoxFragment, Fragment};
use crate::geom::{LogicalSides1D, LogicalVec2, PhysicalPoint, PhysicalRect, PhysicalVec};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::MathLayoutOutput;
use crate::positioned::PositioningContext;
use crate::sizing::{ComputeInlineContentSizes, InlineContentSizesResult, LazySize};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock};

fn is_default_stretchy_char(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')' | '[' | ']' | '{' | '}' | '|' |
         // Unicode Fences & Angle Brackets
         '⟨' | '⟩' | '〈' | '〉' | '⌊' | '⌋' | '⌈' | '⌉' | '‖' | '⦠' | '⦡' |
         // Arrows
         '←' | '→' | '↔' | '⇐' | '⇒' | '⇔' | '⟵' | '⟶' | '⟷' |
         // Integrals, Sums, & Large Operators
         '∫' | '∬' | '∭' | '∮' | '∑' | '∏' | '∐' | '⋃' | '⋂' | '⋁' | '⋀' |
         // Accents / Over-Under Fences
         '‾' | '_' | '⁀' | '⏞' | '⏟' | '⏠' | '⏡'
    )
}
fn collect_text_content<'dom>(info: &NodeAndStyleInfo<'dom>) -> String {
    let mut text = String::new();
    for child in info.node.flat_tree_children() {
        if child.is_text_node() {
            text.push_str(&child.text_content());
        }
    }
    text
}
fn parse_operator_info(info: &NodeAndStyleInfo, text_content: &str) -> (bool, Option<char>) {
    let mut operator_char = None;
    let mut is_stretchy = false;
    if let Some(element) = info.node.as_element() {
        if element.local_name() == &local_name!("mo") {
            operator_char = text_content.trim().chars().next();
            let explicit_stretchy = element.attribute_as_str(&ns!(), &local_name!("stretchy"));
            is_stretchy = match explicit_stretchy {
                Some("true") => true,
                Some("false") => false,
                _ => operator_char.map_or(false, is_default_stretchy_char),
            };
        }
    }
    (is_stretchy, operator_char)
}
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathOperatorBlock {
    pub base: LayoutBoxBase,
    pub inline_formatting_context: InlineFormattingContext,
    pub is_stretchy: bool,
    pub operator_char: Option<char>,
}
impl MathOperatorBlock {
    pub(crate) fn construct(
        info: &NodeAndStyleInfo,
        inline_formatting_context: InlineFormattingContext,
    ) -> Self {
        let text_content = collect_text_content(info);
        let (is_stretchy, operator_char) = parse_operator_info(info, &text_content);
        Self {
            base: LayoutBoxBase::new(info.into(), info.style.clone()),
            inline_formatting_context,
            is_stretchy,
            operator_char,
        }
    }
    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.inline_formatting_context
            .compute_inline_content_sizes(layout_context, constraint_space)
    }
    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<(Au, Au)>,
    ) -> MathLayoutOutput {
        let mut ifc_positioning_context = PositioningContext::default();
        let result = self.inline_formatting_context.layout(
            layout_context,
            &mut ifc_positioning_context,
            containing_block,
            None,
            CollapsibleWithParentStartMargin(false),
            LogicalSides1D::default(),
        );
        positioning_context.append(ifc_positioning_context);
        let style = &self.base.style;
        let writing_mode = style.writing_mode;
        let layout_style = LayoutStyle::Default(style);
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let physical_padding = pbm.padding.to_physical(writing_mode);
        let border = pbm.border.to_physical(writing_mode);
        let margin = pbm.margin.auto_is(Au::zero).to_physical(writing_mode);
        let mut fragments = result.fragments;
        let mut natural_inline_size = Au::zero();
        for frag in &fragments {
            if let Some(base) = frag.base() {
                let rect = base.rect();
                natural_inline_size = natural_inline_size.max(rect.origin.x + rect.size.width);
            }
        }
        if natural_inline_size == Au::zero() {
            natural_inline_size = containing_block.size.inline;
        }
        let natural_baseline = result.baselines.first.unwrap_or(result.content_block_size);
        let (block_size, ascent, descent, baselines) = if self.is_stretchy && target_size.is_some() {
            let (target_ascent, target_descent) = target_size.unwrap();
            let total_block = target_ascent + target_descent;
            // Center operator vertically inside total_block
            let y_shift = (total_block - result.content_block_size) / 2;
            if y_shift != Au::zero() {
                let translation = PhysicalVec::new(Au::zero(), y_shift);
                for frag in &mut fragments {
                    if let Some(base) = frag.base() {
                        base.translate_rect(translation.into());
                    }
                }
            }
            let op_baseline = physical_padding.top + target_ascent;
            let op_descent = target_descent + physical_padding.bottom;
            let baselines = Baselines {
                first: Some(op_baseline),
                last: Some(op_baseline),
            };
            (total_block, op_baseline, op_descent, baselines)
        } else {
            let op_baseline = physical_padding.top + natural_baseline;
            let op_descent = result.content_block_size - natural_baseline + physical_padding.bottom;
            (result.content_block_size, op_baseline, op_descent, result.baselines)
        };
        let logical_size = LogicalVec2 {
            inline: natural_inline_size,
            block: block_size,
        };
        let physical_size = logical_size.to_physical_size(writing_mode);
        let content_rect = PhysicalRect::new(PhysicalPoint::zero(), physical_size);
        let box_fragment = BoxFragment::new(
            self.base.base_fragment_info.clone(),
            self.base.style.clone(),
            fragments,
            content_rect,
            physical_padding,
            border,
            margin,
            None,
        )
        .with_baselines(baselines);
        MathLayoutOutput {
            fragments: vec![Fragment::Box(box_fragment.into())],
            inline_size: natural_inline_size + physical_padding.left + physical_padding.right,
            block_size: block_size + physical_padding.top + physical_padding.bottom,
            ascent,
            descent,
        }
    }
    pub(crate) fn attached_to_tree(&self, _layout_box: WeakLayoutBox) {}
    pub(crate) fn repair_style(
        &mut self,
        _context: &SharedStyleContext,
        _node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
    }
}