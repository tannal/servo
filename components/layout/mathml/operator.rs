/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Operator capability queries and the current text-layout fallback.
//!
//! See [Core operators](https://w3c.github.io/mathml-core/#operator-fence-separator-or-accent-mo)
//! and [glyph stretching](https://w3c.github.io/mathml-core/#algorithms-for-glyph-stretching).
//! Compare [Blink operators](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/layout/mathml/math_operator_layout_algorithm.cc)
//! and [Gecko MathMLChar](https://searchfox.org/mozilla-central/source/layout/mathml/nsMathMLChar.cpp).
//! Real stretching selects a glyph variant or assembles parts, then returns its
//! actual measurements. Merely enlarging a box or recentering text does not do that.
//!
//! The current hardcoded stretchiness defaults and symmetric default are partial
//! operator policy. The intrinsic-axis table is separate and form-independent.
//! Do not interpret either as a complete operator dictionary implementation.

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
use crate::geom::{LogicalSides1D, PhysicalVec};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutResult, MathOperatorProperties, MathStretchAxis, MathStretchTarget,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ComputeInlineContentSizes, InlineContentSizesResult, LazySize};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock};

/// Partial compatibility fallback, not the Core operator dictionary. In
/// particular, dictionary defaults depend on operator form, and a large operator
/// is not automatically a stretchy fence. Keep this limitation visible to callers.
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
/// Collect direct text children for the current operator policy parser.
/// This does not prove that mixed descendant content is a simple glyph token.
fn collect_text_content<'dom>(info: &NodeAndStyleInfo<'dom>) -> String {
    let mut text = String::new();
    for child in info.node.flat_tree_children() {
        if child.is_text_node() {
            text.push_str(&child.text_content());
        }
    }
    text
}
/// Recognize a single trimmed character and an explicit stretchy boolean.
/// The current defaults remain incomplete; normalization and dictionary form
/// resolution must not be inferred from this small parser.
fn parse_operator_info(info: &NodeAndStyleInfo, text_content: &str) -> (bool, Option<char>) {
    let mut operator_char = None;
    let mut is_stretchy = false;
    if let Some(element) = info.node.as_element() {
        if element.local_name() == &local_name!("mo") {
            let mut characters = text_content.trim().chars();
            operator_char = characters.next().filter(|_| characters.next().is_none());
            let explicit_stretchy = element.attribute_as_str(&ns!(), &local_name!("stretchy"));
            is_stretchy = match explicit_stretchy {
                Some(value) if value.eq_ignore_ascii_case("true") => true,
                Some(value) if value.eq_ignore_ascii_case("false") => false,
                _ => operator_char.map_or(false, is_default_stretchy_char),
            };
        }
    }
    (is_stretchy, operator_char)
}
/// Core operator state plus the IFC used by the current fallback implementation.
/// A capability query is cheap structural information; a completed glyph stretch
/// must ultimately come from shaping, not from this state alone.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathOperatorBlock {
    /// Styled operator identity and fragment bookkeeping.
    pub base: LayoutBoxBase,
    /// Actual text layout currently used instead of a stretchy-glyph shaper.
    pub inline_formatting_context: InlineFormattingContext,
    /// Explicit attribute or current partial default lookup, not glyph coverage capability.
    pub is_stretchy: bool,
    /// Single normalized character if recognized; multiple characters disable glyph stretching.
    pub operator_char: Option<char>,
    /// Current symmetry policy; the constructor still uses an incomplete dictionary fallback.
    symmetric: bool,
}
impl MathOperatorBlock {
    /// Capture current DOM-derived properties and retain the IFC used by the
    /// text fallback. Capability queries do not shape a glyph or select a variant.
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
            // Keep the existing symmetric default until dictionary resolution
            // supplies all operator properties; explicit false is respected.
            symmetric: info
                .node
                .as_element()
                .and_then(|element| {
                    element
                        .attribute_as_str(&ns!(), &local_name!("symmetric"))
                        .map(|value| !value.eq_ignore_ascii_case("false"))
                })
                .unwrap_or(true),
        }
    }
    /// Use current IFC intrinsic advances. The complete Core intrinsic contribution
    /// for vertically stretched variants is not implemented by this delegation.
    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.inline_formatting_context
            .compute_inline_content_sizes(layout_context, constraint_space)
    }
    /// Run the text fallback, then interpret a compatible block target at the core
    /// operator. Symmetry uses this operator's font axis, not its parent's font axis.
    /// Current behavior centers natural text inside a larger declared box; it does
    /// not paint a larger glyph. Inline requests remain natural-sized until shaping
    /// can produce actual horizontal variants/assemblies.
    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
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
        let layout_style = LayoutStyle::Default(style);
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
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
        let mut metrics = MathBoxMetrics::new(
            natural_inline_size,
            result.content_block_size,
            natural_baseline,
        );
        if self.is_stretchy
            && self.operator_char.is_some()
            && self.operator_properties().stretch_axis == MathStretchAxis::Block
        {
            if let Some(MathStretchTarget::Block {
                mut ascent,
                mut descent,
            }) = target_size
            {
                // Symmetry belongs to the core operator, not to its parent row.
                if self.symmetric {
                    let font_group = layout_context.font_context.font_group(style.clone_font());
                    let font = font_group.first(&layout_context.font_context);
                    let axis = font
                        .as_ref()
                        .map(|font| {
                            font.math_constants(false)
                                .map(|constants| constants.axis_height)
                                .unwrap_or_else(|| font.metrics().x_height / 2)
                        })
                        .unwrap_or(Au::zero());
                    let half_height = (ascent - axis).max(descent + axis);
                    ascent = half_height + axis;
                    descent = half_height - axis;
                }
                // TODO: Replace the current centering fallback with glyph variant
                // selection/assembly. A zero stretch target retains natural size.
                let target_height = ascent + descent;
                if target_height > metrics.size.block {
                    let y_shift = (target_height - metrics.size.block) / 2;
                    let translation = PhysicalVec::new(Au::zero(), y_shift);
                    for fragment in &mut fragments {
                        if let Some(base) = fragment.base() {
                            base.translate_rect(translation.into());
                        }
                    }
                    metrics = MathBoxMetrics::new(natural_inline_size, target_height, ascent);
                }
            }
        }
        // Inline requests deliberately retain natural measurements until the
        // shaper can produce a genuinely horizontally stretched glyph.
        MathLayoutResult::from_content(&self.base, containing_block, &pbm, fragments, metrics, None)
    }

    /// Report intrinsic axis independently of the stretchy flag. Multi-character
    /// content does not advertise glyph stretching. Wrapper queries use this result
    /// before sibling measurement, avoiding a layout pass just to classify a child.
    pub(crate) fn operator_properties(&self) -> MathOperatorProperties {
        MathOperatorProperties {
            stretchy: self.is_stretchy && self.operator_char.is_some(),
            stretch_axis: if self
                .operator_char
                .is_some_and(|c| INLINE_STRETCH_OPERATORS.binary_search(&(c as u32)).is_ok())
            {
                MathStretchAxis::Inline
            } else {
                MathStretchAxis::Block
            },
        }
    }

    /// Current no-op lifecycle hook; it does not register a separate math paint object.
    pub(crate) fn attached_to_tree(&self, _layout_box: WeakLayoutBox) {}
    /// Update the IFC and, when the supplied DOM node is an mo, refresh the current
    /// operator policy. The node guard accommodates existing ancestor repair calls;
    /// it does not solve the broader child-style lifecycle problem.
    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        // Some existing container repair paths pass the ancestor's node. Only
        // refresh DOM-derived operator properties when this is the actual mo.
        if node
            .as_element()
            .is_some_and(|element| element.local_name() == &local_name!("mo"))
        {
            let info = NodeAndStyleInfo::new(*node, new_style.clone());
            let (is_stretchy, operator_char) =
                parse_operator_info(&info, &collect_text_content(&info));
            self.is_stretchy = is_stretchy;
            self.operator_char = operator_char;
            self.symmetric = node
                .as_element()
                .and_then(|element| {
                    element
                        .attribute_as_str(&ns!(), &local_name!("symmetric"))
                        .map(|value| !value.eq_ignore_ascii_case("false"))
                })
                .unwrap_or(true);
        }
        self.inline_formatting_context
            .repair_style(context, node, new_style);
    }
}

// https://w3c.github.io/mathml-core/#operator-dictionary
// Intrinsic inline stretch axes, independent of operator form or stretchiness.
const INLINE_STRETCH_OPERATORS: &[u32] = &[
    0x003D, 0x005E, 0x005F, 0x007E, 0x00AF, 0x02C6, 0x02C7, 0x02C9, 0x02CD, 0x02DC, 0x02F7, 0x0302,
    0x0332, 0x203E, 0x20D0, 0x20D1, 0x20D6, 0x20D7, 0x20E1, 0x2190, 0x2192, 0x2194, 0x2198, 0x2199,
    0x219A, 0x219B, 0x219C, 0x219D, 0x219E, 0x21A0, 0x21A2, 0x21A3, 0x21A4, 0x21A6, 0x21A9, 0x21AA,
    0x21AB, 0x21AC, 0x21AD, 0x21AE, 0x21B4, 0x21B9, 0x21BC, 0x21BD, 0x21C0, 0x21C1, 0x21C4, 0x21C6,
    0x21C7, 0x21C9, 0x21CB, 0x21CC, 0x21CD, 0x21CE, 0x21CF, 0x21D0, 0x21D2, 0x21D4, 0x21DA, 0x21DB,
    0x21DC, 0x21DD, 0x21E0, 0x21E2, 0x21E4, 0x21E5, 0x21E6, 0x21E8, 0x21F0, 0x21F4, 0x21F6, 0x21F7,
    0x21F8, 0x21F9, 0x21FA, 0x21FB, 0x21FC, 0x21FD, 0x21FE, 0x21FF, 0x2322, 0x2323, 0x23B4, 0x23B5,
    0x23DC, 0x23DD, 0x23DE, 0x23DF, 0x23E0, 0x23E1, 0x2500, 0x2794, 0x2799, 0x279B, 0x279C, 0x279D,
    0x279E, 0x279F, 0x27A0, 0x27A1, 0x27A5, 0x27A6, 0x27A8, 0x27A9, 0x27AA, 0x27AB, 0x27AC, 0x27AD,
    0x27AE, 0x27AF, 0x27B1, 0x27B3, 0x27B5, 0x27B8, 0x27BA, 0x27BB, 0x27BC, 0x27BD, 0x27BE, 0x27F4,
    0x27F5, 0x27F6, 0x27F7, 0x27F8, 0x27F9, 0x27FA, 0x27FB, 0x27FC, 0x27FD, 0x27FE, 0x27FF, 0x2900,
    0x2901, 0x2902, 0x2903, 0x2904, 0x2905, 0x2906, 0x2907, 0x290C, 0x290D, 0x290E, 0x290F, 0x2910,
    0x2911, 0x2914, 0x2915, 0x2916, 0x2917, 0x2918, 0x2919, 0x291A, 0x291B, 0x291C, 0x291D, 0x291E,
    0x291F, 0x2920, 0x2942, 0x2943, 0x2944, 0x2945, 0x2946, 0x2947, 0x2948, 0x294A, 0x294B, 0x294E,
    0x2950, 0x2952, 0x2953, 0x2956, 0x2957, 0x295A, 0x295B, 0x295E, 0x295F, 0x2962, 0x2964, 0x2966,
    0x2967, 0x2968, 0x2969, 0x296A, 0x296B, 0x296C, 0x296D, 0x2970, 0x2971, 0x2972, 0x2973, 0x2974,
    0x2975, 0x297C, 0x297D, 0x2B04, 0x2B05, 0x2B0C, 0x2B30, 0x2B31, 0x2B32, 0x2B33, 0x2B34, 0x2B35,
    0x2B36, 0x2B37, 0x2B38, 0x2B39, 0x2B3A, 0x2B3B, 0x2B3C, 0x2B3D, 0x2B3E, 0x2B40, 0x2B41, 0x2B42,
    0x2B43, 0x2B44, 0x2B45, 0x2B46, 0x2B47, 0x2B48, 0x2B49, 0x2B4A, 0x2B4B, 0x2B4C, 0x2B60, 0x2B62,
    0x2B64, 0x2B6A, 0x2B6C, 0x2B70, 0x2B72, 0x2B7A, 0x2B7C, 0x2B80, 0x2B82, 0x2B84, 0x2B86, 0x2B95,
    0xFE35, 0xFE36, 0xFE37, 0xFE38, 0x1EEF0, 0x1EEF1,
];
