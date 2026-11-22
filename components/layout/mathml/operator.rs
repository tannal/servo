/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Operator capability queries, OpenType MATH glyph stretching (both vertical
//! and horizontal, plus display-style large operators), and text-layout fallback.
//!
//! See [Core operators](https://w3c.github.io/mathml-core/#operator-fence-separator-or-accent-mo)
//! and [glyph stretching](https://w3c.github.io/mathml-core/#algorithms-for-glyph-stretching).

use app_units::Au;
use euclid::num::Zero;
use fonts::MathStretchDirection;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::context::SharedStyleContext;
use style::dom::NodeInfo;
use style::properties::ComputedValues;
use style::values::computed::font::MathStyle;
use web_atoms::{local_name, ns};

use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::flow::CollapsibleWithParentStartMargin;
use crate::flow::inline::InlineFormattingContext;
use crate::fragment_tree::{MathGlyphRun, MathPaintInfo};
use crate::geom::{LogicalSides1D, PhysicalPoint, PhysicalRect, PhysicalSize};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutResult, MathOperatorProperties, MathStretchAxis, MathStretchTarget,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ComputeInlineContentSizes, InlineContentSizesResult};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock};

/// Operators that default to `stretchy="true"` in the MathML Core operator dictionary.
fn is_default_stretchy_char(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')' | '[' | ']' | '{' | '}' | '|' |
         // Unicode Fences & Angle Brackets
         '⟨' | '⟩' | '〈' | '〉' | '⌊' | '⌋' | '⌈' | '⌉' | '‖' | '⦠' | '⦡' |
         // Arrows
         '←' | '→' | '↔' | '⇐' | '⇒' | '⇔' | '⟵' | '⟶' | '⟷' | '↑' | '↓' | '↕' |
         '⇑' | '⇓' | '⇕' | '↦' | '↩' | '↪' | '↼' | '↽' | '⇀' | '⇁' | '⇌' | '⇋' |
         // Accents / Over-Under Fences
         '‾' | '_' | '^' | '~' | 'ˆ' | 'ˇ' | '˜' | '¯' | '⁀' | '⏞' | '⏟' | '⏠' | '⏡' |
         '⎴' | '⎵' | '⏜' | '⏝' | '⏢'
    ) || INLINE_STRETCH_OPERATORS.binary_search(&(ch as u32)).is_ok()
}

/// Operators that default to `largeop="true"` in the MathML Core operator dictionary.
fn is_default_largeop_char(ch: char) -> bool {
    matches!(
        ch,
        '∫' | '∬' | '∭' | '∮' | '∯' | '∰' | '∱' | '∲' | '∳'
            | '∑' | '∏' | '∐'
            | '⋀' | '⋁' | '⋂' | '⋃'
            | '⨀' | '⨁' | '⨂' | '⨃' | '⨄' | '⨅' | '⨆' | '⨇' | '⨈' | '⨉' | '⨊' | '⨋'
            | '⨌' | '⨍' | '⨎' | '⨏' | '⨐' | '⨑' | '⨒' | '⨓' | '⨔' | '⨕' | '⨖' | '⨗' | '⨘'
            | '⨙' | '⨚' | '⨛' | '⨜' | '⫼' | '⫿'
    )
}

/// Operators that default to `movablelimits="true"` in the MathML Core operator dictionary.
fn is_default_movablelimits_char(ch: char) -> bool {
    matches!(
        ch,
        '∑' | '∏' | '∐'
            | '⋀' | '⋁' | '⋂' | '⋃'
            | '⨀' | '⨁' | '⨂' | '⨃' | '⨄' | '⨅' | '⨆' | '⨇' | '⨈' | '⨉'
    )
}

/// Operators that default to `accent="true"` in the MathML Core operator dictionary.
fn is_default_accent_char(ch: char) -> bool {
    matches!(
        ch,
        '^' | '_' | '~' | '¯' | '´' | '`' | '¨' | '˙' | 'ˆ' | 'ˇ' | 'ˉ' | '˘' | '˚' | '˜'
            | '‾' | '⏞' | '⏟' | '⏠' | '⏡' | '⏜' | '⏝' | '→' | '←' | '↔'
            | '\u{0300}'..='\u{036F}'
            | '\u{20D0}'..='\u{20FF}'
    )
}

/// Operators that default to `symmetric="true"` in the MathML Core operator dictionary.
fn is_default_symmetric_char(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')' | '[' | ']' | '{' | '}' | '|' | '‖'
            | '⟨' | '⟩' | '〈' | '〉' | '⌊' | '⌋' | '⌈' | '⌉'
            | '∫' | '∬' | '∭' | '∮' | '∯' | '∰' | '∑' | '∏' | '∐'
            | '↑' | '↓' | '↕' | '⇑' | '⇓' | '⇕'
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

struct ParsedOperatorFlags {
    operator_char: Option<char>,
    stretchy: bool,
    symmetric: bool,
    largeop: bool,
    movablelimits: bool,
    accent: bool,
}

fn parse_bool_attr<'dom, E: LayoutElement<'dom>>(
    element: &E,
    attr: &web_atoms::LocalName,
) -> Option<bool> {
    let value = element.attribute_as_str(&ns!(), attr)?;
    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn parse_operator_flags(info: &NodeAndStyleInfo, text_content: &str) -> ParsedOperatorFlags {
    let mut flags = ParsedOperatorFlags {
        operator_char: None,
        stretchy: false,
        symmetric: false,
        largeop: false,
        movablelimits: false,
        accent: false,
    };
    let Some(element) = info.node.as_element() else {
        return flags;
    };
    if element.local_name() != &local_name!("mo") {
        return flags;
    }

    let trimmed = text_content.trim();
    let mut characters = trimmed.chars();
    let ch = characters.next().filter(|_| characters.next().is_none());
    flags.operator_char = ch;

    flags.stretchy = parse_bool_attr(&element, &local_name!("stretchy"))
        .unwrap_or_else(|| ch.is_some_and(is_default_stretchy_char));
    flags.symmetric = parse_bool_attr(&element, &local_name!("symmetric"))
        .unwrap_or_else(|| ch.is_some_and(is_default_symmetric_char));
    flags.largeop = parse_bool_attr(&element, &local_name!("largeop"))
        .unwrap_or_else(|| ch.is_some_and(is_default_largeop_char));
    flags.movablelimits = parse_bool_attr(&element, &local_name!("movablelimits"))
        .unwrap_or_else(|| ch.is_some_and(is_default_movablelimits_char));
    flags.accent = parse_bool_attr(&element, &local_name!("accent"))
        .unwrap_or_else(|| ch.is_some_and(is_default_accent_char));

    // Special case: multi-char operators like "lim", "max", "min", "sup", "inf", "det", "gcd"
    // default to movablelimits="true" per the operator dictionary.
    if ch.is_none()
        && parse_bool_attr(&element, &local_name!("movablelimits")).is_none()
        && matches!(
            trimmed,
            "lim" | "liminf" | "limsup" | "max" | "min" | "sup" | "inf" | "det" | "gcd" | "Pr"
        )
    {
        flags.movablelimits = true;
    }

    flags
}

/// Core operator state plus the IFC used when not painting a stretched glyph.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathOperatorBlock {
    pub base: LayoutBoxBase,
    pub inline_formatting_context: InlineFormattingContext,
    pub is_stretchy: bool,
    pub operator_char: Option<char>,
    symmetric: bool,
    largeop: bool,
    movablelimits: bool,
    accent: bool,
}

impl MathOperatorBlock {
    pub(crate) fn construct(
        info: &NodeAndStyleInfo,
        inline_formatting_context: InlineFormattingContext,
    ) -> Self {
        let text_content = collect_text_content(info);
        let flags = parse_operator_flags(info, &text_content);
        Self {
            base: LayoutBoxBase::new(info.into(), info.style.clone()),
            inline_formatting_context,
            is_stretchy: flags.stretchy,
            operator_char: flags.operator_char,
            symmetric: flags.symmetric,
            largeop: flags.largeop,
            movablelimits: flags.movablelimits,
            accent: flags.accent,
        }
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base.cached_inline_content_sizes_with(constraint_space, || {
            let mut result = self
                .inline_formatting_context
                .compute_inline_content_sizes(layout_context, constraint_space);
            // If this is a largeop in display style, ensure intrinsic width accounts for the display variant.
            if self.largeop
                && self.base.style.get_font().math_style == MathStyle::Normal
                && let Some(ch) = self.operator_char
            {
                let font_group = layout_context
                    .font_context
                    .font_group(self.base.style.clone_font());
                if let Some(font) = font_group.first(&layout_context.font_context) {
                    let min_height = font
                        .math_constants(true)
                        .map(|c| c.display_operator_min_height)
                        .unwrap_or_else(|| {
                            Au::from_f32_px(self.base.style.get_font().font_size.computed_size().px() * 1.414)
                        });
                    if let Some(stretched) =
                        font.math_stretch_glyph(ch, MathStretchDirection::Vertical, min_height)
                    {
                        let pbm = LayoutStyle::Default(&self.base.style)
                            .content_box_sizes_and_padding_border_margin(&constraint_space.into())
                            .pbm;
                        let extra = pbm.padding_border_sums.inline
                            + pbm.margin.auto_is(Au::zero).inline_sum();
                        let width = stretched.inline_size + extra;
                        result.sizes.min_content = result.sizes.min_content.max(width);
                        result.sizes.max_content = result.sizes.max_content.max(width);
                    }
                }
            }
            result
        })
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let style = &self.base.style;
        let layout_style = LayoutStyle::Default(style);
        let pbm = layout_style
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let is_display_style = style.get_font().math_style == MathStyle::Normal;
        let props = self.operator_properties();

        // Check if we should paint via OpenType MATH glyph stretching:
        // 1. Display-style largeop (`largeop="true"` and `math-style: normal`)
        // 2. Stretchy block operator with a Block target
        // 3. Stretchy inline operator with an Inline target
        if let Some(ch) = self.operator_char {
            let font_group = layout_context.font_context.font_group(style.clone_font());
            if let Some(font) = font_group.first(&layout_context.font_context) {
                let axis = font
                    .math_constants(is_display_style)
                    .map(|c| c.axis_height)
                    .unwrap_or_else(|| font.metrics().x_height / 2);

                let stretch_request: Option<(MathStretchDirection, Au, Option<(Au, Au)>)> =
                    if self.largeop && is_display_style {
                        let min_height = font
                            .math_constants(true)
                            .map(|c| c.display_operator_min_height)
                            .filter(|&h| h > Au::zero())
                            .unwrap_or_else(|| {
                                Au::from_f32_px(
                                    style.get_font().font_size.computed_size().px() * 1.414,
                                )
                            });
                        Some((MathStretchDirection::Vertical, min_height, None))
                    } else if props.stretchy {
                        match (props.stretch_axis, target_size) {
                            (
                                MathStretchAxis::Block,
                                Some(MathStretchTarget::Block {
                                    mut ascent,
                                    mut descent,
                                }),
                            ) => {
                                if self.symmetric {
                                    let half = (ascent - axis).max(descent + axis);
                                    ascent = half + axis;
                                    descent = half - axis;
                                }
                                let total = (ascent + descent).max(Au::zero());
                                (total > Au::zero()).then_some((
                                    MathStretchDirection::Vertical,
                                    total,
                                    Some((ascent, descent)),
                                ))
                            },
                            (MathStretchAxis::Inline, Some(MathStretchTarget::Inline(width)))
                                if width > Au::zero() =>
                            {
                                Some((MathStretchDirection::Horizontal, width, None))
                            },
                            _ => None,
                        }
                    } else {
                        None
                    };

                if let Some((direction, target_span, block_target)) = stretch_request
                    && let Some(stretched) = font.math_stretch_glyph(ch, direction, target_span)
                {
                    let glyph_height = (stretched.ascent + stretched.descent).max(Au::zero());
                    let (final_ascent, final_descent, glyph_baseline_y) =
                        if direction == MathStretchDirection::Vertical {
                            if self.largeop && is_display_style {
                                // Center large operator vertically around the math axis.
                                let half = glyph_height / 2;
                                let ascent = half + axis;
                                let descent = glyph_height - ascent;
                                let baseline_y = ascent - (stretched.ascent - half) - axis
                                    + (stretched.ascent - half);
                                // Exact placement: top of glyph ink is at `glyph_baseline_y - stretched.ascent = 0`.
                                let _ = baseline_y;
                                (ascent, descent, stretched.ascent)
                            } else if let Some((req_ascent, req_descent)) = block_target {
                                if self.symmetric {
                                    let half = glyph_height / 2;
                                    let ascent = half + axis;
                                    let descent = glyph_height - ascent;
                                    (ascent, descent, stretched.ascent)
                                } else {
                                    // Align to cover requested ascent/descent.
                                    let ascent = req_ascent.max(stretched.ascent);
                                    let descent = (glyph_height - ascent).max(req_descent);
                                    let y_center_shift =
                                        ((ascent + descent) - glyph_height) / 2;
                                    (ascent, descent, y_center_shift + stretched.ascent)
                                }
                            } else {
                                (stretched.ascent, stretched.descent, stretched.ascent)
                            }
                        } else {
                            (stretched.ascent, stretched.descent, stretched.ascent)
                        };

                    let content_block = (final_ascent + final_descent).max(Au::zero());
                    let content_inline = stretched.inline_size.max(Au::zero());
                    let font_key =
                        font.key(layout_context.painter_id, &layout_context.font_context);
                    let paint_run = MathGlyphRun {
                        font_key,
                        glyphs: stretched.glyphs,
                        origin: PhysicalPoint::new(Au::zero(), glyph_baseline_y),
                        bounds: PhysicalRect::new(
                            PhysicalPoint::new(Au::zero(), Au::zero()),
                            PhysicalSize::new(content_inline, content_block),
                        ),
                    };

                    let mut metrics =
                        MathBoxMetrics::new(content_inline, content_block, final_ascent);
                    metrics.italic_correction = stretched.italics_correction;
                    metrics.top_accent_attachment = stretched.top_accent_attachment;

                    return MathLayoutResult::from_content(
                        &self.base,
                        containing_block,
                        &pbm,
                        Vec::new(),
                        metrics,
                        Some(MathPaintInfo::StretchedGlyph(paint_run)),
                    );
                }
            }
        }

        // Standard unstretched IFC text layout path.
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

        let fragments = result.fragments;
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

        // Populate tight ink extents, italic correction, and top accent attachment
        // from HarfBuzz when this is a single-character operator.
        if let Some(ch) = self.operator_char {
            let font_group = layout_context.font_context.font_group(style.clone_font());
            if let Some(font) = font_group.first(&layout_context.font_context) {
                if let Some(ext) = font.math_glyph_extents(ch) {
                    metrics.ink_ascent = ext.ascent;
                    metrics.ink_descent = ext.descent;
                }
                metrics.italic_correction = font.math_char_italics_correction(ch);
                metrics.top_accent_attachment = font.math_char_top_accent_attachment(ch);
            }
        }

        MathLayoutResult::from_content(&self.base, containing_block, &pbm, fragments, metrics, None)
    }

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
            largeop: self.largeop,
            movablelimits: self.movablelimits,
            accent: self.accent,
        }
    }

    pub(crate) fn attached_to_tree(&self, _layout_box: WeakLayoutBox) {}

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        if node
            .as_element()
            .is_some_and(|element| element.local_name() == &local_name!("mo"))
        {
            let info = NodeAndStyleInfo::new(*node, new_style.clone());
            let flags = parse_operator_flags(&info, &collect_text_content(&info));
            self.is_stretchy = flags.stretchy;
            self.operator_char = flags.operator_char;
            self.symmetric = flags.symmetric;
            self.largeop = flags.largeop;
            self.movablelimits = flags.movablelimits;
            self.accent = flags.accent;
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
