/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! CSS-flow integration for mathematical tokens (`mi`, `mn`, `ms`, `mtext`),
//! MathML tables (`mtable`), and embedded non-math CSS/replaced elements.
//!
//! See [Core token layout](https://w3c.github.io/mathml-core/#layout-of-mtext)
//! and [Core tables](https://w3c.github.io/mathml-core/#tabular-math).

use std::sync::Arc;

use app_units::Au;
use layout_api::LayoutNode;
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::Zero;
use style::context::SharedStyleContext;
use style::dom::NodeInfo;
use style::properties::ComputedValues;
use style::values::computed::font::MathStyle;

use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::flow::IndependentFloatOrAtomicLayoutResult;
use crate::formatting_contexts::IndependentFormattingContext;
use crate::fragment_tree::Fragment;
use crate::geom::{AuOrAuto, LogicalVec2, ToLogical};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{MathBoxMetrics, MathLayoutResult, MathStretchTarget};
use crate::positioned::{PositioningContext, PositioningContextLength};
use crate::sizing::InlineContentSizesResult;
use crate::style_ext::{ComputedValuesExt, PaddingBorderMargin};
use crate::{ConstraintSpace, ContainingBlock};

/// Extract the single character of a simple text-only token element (e.g. `<mi>x</mi>`).
/// Returns `None` if the token has element children or more than one non-whitespace char.
fn extract_single_token_char<'dom>(info: &NodeAndStyleInfo<'dom>) -> Option<char> {
    let mut text = String::new();
    for child in info.node.flat_tree_children() {
        if child.is_text_node() {
            text.push_str(&child.text_content());
        } else {
            return None;
        }
    }
    let mut chars = text.trim().chars();
    chars.next().filter(|_| chars.next().is_none())
}

/// Math-token or embedded CSS/table wrapper around ordinary CSS layout.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathTextBlock {
    /// Token/element identity and style.
    pub base: LayoutBoxBase,
    /// CSS formatting context (Flow, Table, Flex, Grid, or Replaced).
    pub formatting_context: IndependentFormattingContext,
    /// Structural `mtext` space-like classification.
    pub is_space_like: bool,
    /// Single character for text-only math tokens, used for tight glyph ink
    /// extents, italic correction, and top accent attachment.
    pub single_char: Option<char>,
    /// True for `<mtable>`: centers the box's baseline on the mathematical axis
    /// per MathML Core §3.5 (`baseline = content_block / 2 + axis_height`).
    pub is_mtable: bool,
}

impl MathTextBlock {
    pub(crate) fn construct(
        info: &NodeAndStyleInfo,
        formatting_context: IndependentFormattingContext,
        is_space_like: bool,
        is_token: bool,
        is_mtable: bool,
    ) -> Self {
        let single_char = is_token.then(|| extract_single_token_char(info)).flatten();
        Self {
            base: LayoutBoxBase::new(info.into(), info.style.clone()),
            formatting_context,
            is_space_like,
            single_char,
            is_mtable,
        }
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base.cached_inline_content_sizes_with(constraint_space, || {
            self.formatting_context.outer_inline_content_sizes(
                layout_context,
                &constraint_space.into(),
                &LogicalVec2::zero(),
                false,
                false,
            )
        })
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        _target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let mut child_positioning_context = PositioningContext::default();
        let IndependentFloatOrAtomicLayoutResult {
            mut fragment,
            baselines,
            pbm_sums,
        } = self.formatting_context.layout_float_or_atomic_inline(
            layout_context,
            &mut child_positioning_context,
            containing_block,
        );
        let writing_mode = containing_block.style.writing_mode;
        let content_size = fragment.content_rect().size.to_logical(writing_mode);

        let content_baseline = if self.is_mtable {
            // MathML Core §3.5: an mtable's baseline is centered on the math axis.
            let is_display = self.base.style.get_font().math_style == MathStyle::Normal;
            let font_group = layout_context
                .font_context
                .font_group(self.base.style.clone_font());
            let axis_height = font_group
                .first(&layout_context.font_context)
                .map(|font| {
                    font.math_constants(is_display)
                        .map(|c| c.axis_height)
                        .unwrap_or_else(|| font.metrics().x_height / 2)
                })
                .unwrap_or_default();
            content_size.block / 2 + axis_height
        } else {
            baselines.first.unwrap_or(content_size.block)
        };

        let padding = fragment.padding.to_logical(writing_mode);
        let border = fragment.border.to_logical(writing_mode);
        let pbm = PaddingBorderMargin {
            padding,
            border,
            margin: fragment
                .margin
                .to_logical(writing_mode)
                .map(|value| AuOrAuto::LengthPercentage(*value)),
            padding_border_sums: (padding + border).sum(),
        };

        let mut content_metrics =
            MathBoxMetrics::new(content_size.inline, content_size.block, content_baseline);

        // For single-character math tokens (e.g. `<mi>x</mi>`, `<mn>2</mn>`),
        // refine ink extents and populate italic correction / top accent attachment
        // from the OpenType MATH table and HarfBuzz glyph extents.
        if let Some(ch) = self.single_char {
            let font_group = layout_context
                .font_context
                .font_group(self.base.style.clone_font());
            if let Some(font) = font_group.first(&layout_context.font_context) {
                if let Some(ext) = font.math_glyph_extents(ch) {
                    content_metrics.ink_ascent = ext.ascent;
                    content_metrics.ink_descent = ext.descent;
                }
                content_metrics.italic_correction = font.math_char_italics_correction(ch);
                content_metrics.top_accent_attachment = font.math_char_top_accent_attachment(ch);
            }
        }

        let metrics = content_metrics.with_padding_border_margin(&pbm);

        fragment
            .base
            .translate_rect(pbm_sums.start_offset().to_physical_size(writing_mode));
        let final_baselines = if self.is_mtable {
            crate::formatting_contexts::Baselines {
                first: Some(content_baseline),
                last: Some(content_baseline),
            }
        } else {
            baselines
        };
        fragment = fragment.with_baselines(final_baselines);
        if fragment
            .style()
            .establishes_containing_block_for_absolute_descendants(fragment.base.flags)
        {
            child_positioning_context.layout_collected_children(layout_context, &mut fragment);
        }
        let fragment = Fragment::Box(Arc::new(fragment));
        child_positioning_context.adjust_static_position_of_hoisted_fragments(
            &fragment,
            PositioningContextLength::zero(),
        );
        positioning_context.append(child_positioning_context);

        MathLayoutResult {
            fragments: vec![fragment],
            metrics,
        }
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        self.formatting_context.attached_to_tree(layout_box);
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        self.formatting_context
            .repair_style(context, node, new_style);
    }
}
