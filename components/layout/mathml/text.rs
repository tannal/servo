/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! CSS-flow integration for mathematical tokens, including embedded content.
//!
//! [Core token layout](https://w3c.github.io/mathml-core/#layout-of-mtext) distinguishes
//! text-only measurement from more general token content. This implementation
//! currently uses the atomic CSS-flow path; returning an ink field does not make
//! that path a shaped-glyph ink measurement.
//! Compare [Blink token measurement](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/layout/mathml/math_token_layout_algorithm.cc),
//! which obtains glyph bounds and positions its inline result against that baseline.
//! A future Servo path must retain actual shaping/fallback-font information rather
//! than guessing ink from font ascent or recursively unioning painted overflow.

use std::sync::Arc;

use app_units::Au;
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::Zero;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;

use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
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

/// Math-token wrapper around ordinary CSS layout. Structural token identity is
/// retained even when content uses HTML/CSS layout; measured size and space-like
/// classification are intentionally independent.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathTextBlock {
    /// Token identity/style; retained independently of the CSS content implementation.
    pub base: LayoutBoxBase,
    /// CSS-flow engine responsible for token descendants, including embedded HTML.
    pub formatting_context: IndependentFormattingContext,
    /// Structural mtext classification; does not mean the returned dimensions are zero.
    pub is_space_like: bool,
}

impl MathTextBlock {
    /// Ask the atomic CSS wrapper for outer contributions, including used sizing
    /// properties and decorations; a math parent must not add them a second time.
    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        // Math rows consume their children's margin-box sizes. Include CSS sizing
        // properties and decorations here, as well as during layout.
        self.formatting_context.outer_inline_content_sizes(
            layout_context,
            &constraint_space.into(),
            &LogicalVec2::zero(),
            false,
            false,
        )
    }

    /// Resolve the atomic CSS box once and adapt its measurements to the math
    /// contract. Keep its fragment baseline content-relative and return the outer
    /// baseline to the parent. Preserve resolved auto margins when constructing
    /// clearance extents. Handle locally owned positioned descendants before appending
    /// remaining positioning state to the parent collector.
    ///
    /// This path uses content-edge clearance as a fallback. A tall CSS line box can
    /// still affect token layout; this function does not obtain tight glyph ink bounds.
    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        _target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        // TODO: Text-only tokens without break opportunities need the ink-metric
        // path specified by MathML Core, rather than CSS line-box metrics.
        // IndependentFormattingContext::layout lays out content only. Use the
        // atomic-box wrapper to resolve CSS sizes and create the styled box,
        // keeping child fragments relative to its content area.
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
        let size = content_size + pbm_sums.sum();
        let ascent = baselines
            .first
            .map_or(size.block, |baseline| pbm_sums.block_start + baseline);
        // Reuse the atomic layout's resolved decorations, including used auto
        // margins, rather than resolving the CSS box a second time.
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
        let metrics = MathBoxMetrics::new(
            content_size.inline,
            content_size.block,
            ascent - pbm_sums.block_start,
        )
        .with_padding_border_margin(&pbm);

        // The math parent places the margin box. BoxFragment stores the content
        // rectangle, and its baselines are relative to that content rectangle.
        fragment
            .base
            .translate_rect(pbm_sums.start_offset().to_physical_size(writing_mode));
        fragment = fragment.with_baselines(baselines);
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
            // CSS-flow fallback uses content edges as ink baselines. A dedicated
            // text-only token path must supply shaped glyph bounds instead.
            metrics,
        }
    }

    /// Forward ownership registration to the ordinary CSS formatting context.
    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        self.formatting_context.attached_to_tree(layout_box);
    }

    /// Repair both the math wrapper and its delegated CSS formatting context;
    /// space-like classification depends on token identity rather than measured size.
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
