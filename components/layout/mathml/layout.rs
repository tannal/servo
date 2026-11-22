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

use crate::containing_block::{ContainingBlock, ContainingBlockSize};
use crate::formatting_contexts::Baselines;
use crate::fragment_tree::{BoxFragment, Fragment, MathPaintInfo};
use crate::geom::{LogicalRect, LogicalVec2};
use crate::layout_box_base::LayoutBoxBase;
use crate::style_ext::PaddingBorderMargin;

/// Requested coverage, not a forced CSS size. The operator owns symmetry,
/// min/max-size resolution, and the choice of a glyph variant or assembly.
/// An absent target requests natural layout; an explicit zero target is still a
/// stretch request. Keep those cases distinct during the all-stretchy row pass.
/// Inline coverage is needed by future under/over algorithms; the current operator
/// path does not implement horizontal glyph stretching.
///
/// References: [Core layout inputs](https://w3c.github.io/mathml-core/#layout-algorithms),
/// [Blink constraints](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/layout/constraint_space.h),
/// and Gecko's `Stretch` in the interface linked from the parent module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MathStretchTarget {
    /// Requested inline coverage; not a replacement for CSS width.
    Inline(Au),
    /// Requested baseline-relative block coverage, preserving asymmetry.
    /// `ascent` covers the line-over side; `descent` covers the line-under side.
    /// Both use the operator's alphabetic baseline as their common reference.
    Block { ascent: Au, descent: Au },
}

/// Intrinsic operator direction in logical coordinates, independent of whether
/// stretching is enabled. A stretchy flag alone cannot identify which row pass
/// should defer an operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MathStretchAxis {
    /// Along the inline progression, for example a horizontal arrow.
    Inline,
    /// Along the block dimension, for example a parenthesis.
    Block,
}

/// Structural information needed before a parent chooses a layout order.
/// Resolve it by querying the core operator, not by measuring its fragments.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MathOperatorProperties {
    /// Intrinsic axis of the core operator, not a measurement of this wrapper.
    pub stretch_axis: MathStretchAxis,
    /// Whether current operator policy permits stretching; false still permits natural layout.
    pub stretchy: bool,
    /// Whether the operator has `largeop="true"` (or dictionary default).
    pub largeop: bool,
    /// Whether the operator has `movablelimits="true"` (or dictionary default).
    pub movablelimits: bool,
    /// Whether the operator has `accent="true"` (or dictionary default).
    pub accent: bool,
    /// Resolved leading space (`lspace`) of the core `<mo>` operator.
    pub lspace: Au,
    /// Resolved trailing space (`rspace`) of the core `<mo>` operator.
    pub rspace: Au,
}

/// Measurements in the parent's logical coordinate system. Returned results
/// describe a margin box; algorithms construct content-box metrics first.
/// Every distance is in Au after font scaling, not OpenType design units or
/// TeX points. A measured zero is valid and does not indicate a missing metric.
///
/// `baseline` is relative to block-start. Ink distances are measured from the
/// alphabetic baseline and are signed. These are the current mathematical
/// clearance measurements, not painted overflow (shadows, transforms, etc.).
#[derive(Clone, Copy, Debug)]
pub(crate) struct MathBoxMetrics {
    /// Declared logical size: content-local during construction, margin-box size on return.
    pub size: LogicalVec2<Au>,
    /// Alphabetic baseline offset from this measurement box's block-start edge.
    pub baseline: Au,
    /// Signed extent above the alphabetic baseline used for mathematical clearance.
    pub ink_ascent: Au,
    /// Signed extent below the alphabetic baseline used for mathematical clearance.
    pub ink_descent: Au,
    /// OpenType MATH italic correction of the base glyph/operator, used by
    /// scripts (`msub`/`msup`/`msubsup`/`mmultiscripts`) and under/over limits.
    pub italic_correction: Au,
    /// Optional OpenType MATH top accent attachment point relative to the box's
    /// inline-start edge, used by `mover`/`munderover` with `accent="true"`.
    pub top_accent_attachment: Option<Au>,
}

impl MathBoxMetrics {
    /// Initialize line and clearance extents from the declared box edges. This is
    /// a fallback constructor, not a glyph-bounds query; a token producer must replace
    /// the ink fields when it obtains real shaping measurements.
    pub fn new(inline_size: Au, block_size: Au, baseline: Au) -> Self {
        Self {
            size: LogicalVec2 {
                inline: inline_size,
                block: block_size,
            },
            baseline,
            ink_ascent: baseline,
            ink_descent: block_size - baseline,
            italic_correction: Au::zero(),
            top_accent_attachment: None,
        }
    }

    /// Distance from the alphabetic baseline to the line-over box edge in the
    /// currently supported orientation; this is not the mathematical axis height.
    pub fn ascent(&self) -> Au {
        self.baseline
    }

    /// Complement of the baseline offset within the declared block size. Derive it
    /// instead of storing a second value that can disagree after resizing.
    pub fn descent(&self) -> Au {
        self.size.block - self.baseline
    }

    /// Convert content measurements to margin-box measurements exactly once.
    /// A positive border establishes the corresponding ink extent at its outer
    /// edge. Negative resolved margins remain signed. No margin collapsing is performed.
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
        if let Some(accent) = &mut self.top_accent_attachment {
            *accent += padding_border.inline_start + margin.inline_start;
        }
        self.size += padding_border.sum() + margin.sum();
        self.baseline += padding_border.block_start + margin.block_start;
        self
    }
}

/// A completed layout, with measurements available without traversing fragments.
/// Paint-only information remains on the ordinary BoxFragment's rare data.
#[derive(Clone)]
pub(crate) struct MathLayoutResult {
    /// Accepted fragment subtree; only the caller decides its placement among siblings.
    pub fragments: Vec<Fragment>,
    /// Measurements corresponding to the returned subtree, available without walking it.
    pub metrics: MathBoxMetrics,
}

/// Incremental layout cache for a `MathLevelBox`.
/// Keyed by `(ContainingBlockSize, Option<MathStretchTarget>)` and invalidated
/// automatically whenever `LayoutBoxBase::is_cached_layout_result_dirty()` is true.
#[derive(Default, malloc_size_of_derive::MallocSizeOf)]
pub(crate) struct MathLayoutCache {
    #[ignore_malloc_size_of = "Cached layout result references fragments already measured in the fragment tree"]
    entry: atomic_refcell::AtomicRefCell<Option<MathLayoutCacheEntry>>,
}

struct MathLayoutCacheEntry {
    containing_block_size: ContainingBlockSize,
    target_size: Option<MathStretchTarget>,
    result: MathLayoutResult,
    positioning_context: crate::positioned::PositioningContext,
}

impl std::fmt::Debug for MathLayoutCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MathLayoutCache").finish_non_exhaustive()
    }
}

impl MathLayoutCache {
    pub(crate) fn get(
        &self,
        base: &LayoutBoxBase,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
        positioning_context: &mut crate::positioned::PositioningContext,
    ) -> Option<MathLayoutResult> {
        if base.is_cached_layout_result_dirty() {
            return None;
        }
        let guard = self.entry.borrow();
        let entry = guard.as_ref()?;
        if entry.containing_block_size == containing_block.size
            && entry.target_size == target_size
        {
            positioning_context.append(entry.positioning_context.clone());
            return Some(entry.result.clone());
        }
        None
    }

    pub(crate) fn store(
        &self,
        base: &LayoutBoxBase,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
        result: &MathLayoutResult,
        positioning_context: &crate::positioned::PositioningContext,
    ) {
        base.clear_cached_layout_result_dirty();
        *self.entry.borrow_mut() = Some(MathLayoutCacheEntry {
            containing_block_size: containing_block.size.clone(),
            target_size,
            result: result.clone(),
            positioning_context: positioning_context.clone(),
        });
    }

    pub(crate) fn clear(&self) {
        *self.entry.borrow_mut() = None;
    }
}

impl MathLayoutResult {
    /// Finish a math content box. Child fragments and paint geometry are relative
    /// to its content origin; the returned measurements describe its margin box.
    /// `metrics` describes undecorated content on entry. `children` and `paint_info`
    /// use that content origin. This function inserts the content inset, stores a
    /// content-relative fragment baseline, and returns outer measurements. Call it
    /// once per styled box; inferred rows used inside mpadded bypass it.
    ///
    /// The contract follows Servo's existing physical/logical conversion support;
    /// full orthogonal and reversed-writing-mode baseline handling is not implied.
    /// [Blink's paint payload](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/layout/mathml/mathml_paint_info.h)
    /// is a related separation: geometry for additional drawing accompanies a normal
    /// CSS fragment rather than introducing a separately styled fraction-bar child.
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
        .as_physical_deprecated(containing_block);
        let fragment = BoxFragment::new(
            base.base_fragment_info.clone(),
            base.style.clone(),
            crate::fragment_tree::FragmentChildren::PhysicalDeprecated(children),
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
