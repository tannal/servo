/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The parent/child contract for mathematical layout.
//!
//! Parents query structural properties before layout, pass stretch constraints
//! down, and consume measured results on return. Siblings are coordinated by
//! their parent; they do not mutate each other's layout state.

use app_units::Au;
use style::Zero;

use crate::ContainingBlock;
use crate::formatting_contexts::Baselines;
use crate::fragment_tree::{BoxFragment, Fragment, MathPaintInfo};
use crate::geom::{LogicalRect, LogicalVec2};
use crate::layout_box_base::LayoutBoxBase;
use crate::style_ext::PaddingBorderMargin;

/// Requested coverage, not a forced CSS size. The operator owns symmetry,
/// min/max-size resolution, and the choice of a glyph variant or assembly.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MathStretchTarget {
    Inline(Au),
    Block { ascent: Au, descent: Au },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MathStretchAxis {
    Inline,
    Block,
}

/// Structural information needed before a parent chooses a layout order.
/// Resolve it by querying the core operator, not by measuring its fragments.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MathOperatorProperties {
    pub stretch_axis: MathStretchAxis,
    pub stretchy: bool,
}

/// Measurements in the parent's logical coordinate system. Returned results
/// describe a margin box; algorithms construct content-box metrics first.
///
/// `baseline` is relative to block-start. Ink distances are measured from the
/// alphabetic baseline and are signed. They follow MathML's ink-baseline rules,
/// rather than representing painted overflow (shadows, transforms, etc.).
#[derive(Clone, Copy, Debug)]
pub(crate) struct MathBoxMetrics {
    pub size: LogicalVec2<Au>,
    pub baseline: Au,
    pub ink_ascent: Au,
    pub ink_descent: Au,
}

impl MathBoxMetrics {
    /// The default for a box without separately established ink baselines.
    /// Token layout should supply glyph ink metrics when that path is available.
    pub fn new(inline_size: Au, block_size: Au, baseline: Au) -> Self {
        Self {
            size: LogicalVec2 {
                inline: inline_size,
                block: block_size,
            },
            baseline,
            ink_ascent: baseline,
            ink_descent: block_size - baseline,
        }
    }

    pub fn ascent(&self) -> Au {
        self.baseline
    }

    pub fn descent(&self) -> Au {
        self.size.block - self.baseline
    }

    /// Convert content measurements to margin-box measurements exactly once.
    /// A positive border establishes the corresponding ink extent at its outer
    /// edge. Include the resolved margins in the parent's clearance measurements
    /// so adjacent notation cannot consume the child's requested outer spacing.
    pub fn with_padding_border_margin(mut self, pbm: &PaddingBorderMargin) -> Self {
        let margin = pbm.margin.auto_is(Au::zero);
        let padding_border = pbm.padding + pbm.border;
        if pbm.border.block_start > Au::zero() {
            self.ink_ascent = self.ascent() + padding_border.block_start;
        }
        if pbm.border.block_end > Au::zero() {
            self.ink_descent = self.descent() + padding_border.block_end;
        }
        self.ink_ascent += margin.block_start;
        self.ink_descent += margin.block_end;
        self.size += padding_border.sum() + margin.sum();
        self.baseline += padding_border.block_start + margin.block_start;
        self
    }
}

/// A completed layout, with measurements available without traversing fragments.
/// Paint-only information remains on the ordinary BoxFragment's rare data.
pub(crate) struct MathLayoutResult {
    pub fragments: Vec<Fragment>,
    pub metrics: MathBoxMetrics,
}

impl MathLayoutResult {
    /// Finish a math content box. Child fragments and paint geometry are relative
    /// to its content origin; the returned measurements describe its margin box.
    pub fn from_content(
        base: &LayoutBoxBase,
        containing_block: &ContainingBlock,
        pbm: &PaddingBorderMargin,
        children: Vec<Fragment>,
        metrics: MathBoxMetrics,
        paint_info: Option<MathPaintInfo>,
    ) -> Self {
        let writing_mode = base.style.writing_mode;
        let margin = pbm.margin.auto_is(Au::zero);
        let insets = pbm.padding + pbm.border + margin;
        let content_rect = LogicalRect {
            start_corner: LogicalVec2 {
                inline: insets.inline_start,
                block: insets.block_start,
            },
            size: metrics.size,
        }
        .as_physical(Some(containing_block));
        let fragment = BoxFragment::new(
            base.base_fragment_info.clone(),
            base.style.clone(),
            children,
            content_rect,
            pbm.padding.to_physical(writing_mode),
            pbm.border.to_physical(writing_mode),
            margin.to_physical(writing_mode),
            None,
        )
        .with_baselines(Baselines {
            first: Some(metrics.baseline),
            last: Some(metrics.baseline),
        })
        .with_math_paint_info(paint_info);
        Self {
            fragments: vec![Fragment::Box(fragment.into())],
            metrics: metrics.with_padding_border_margin(pbm),
        }
    }
}
