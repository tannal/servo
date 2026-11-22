/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Operator capability queries, W3C MathML Core operator dictionary lookup,
//! OpenType MATH glyph stretching (vertical, horizontal, and display-style largeop),
//! and text-layout fallback.
//!
//! See [Core operators](https://w3c.github.io/mathml-core/#operator-fence-separator-or-accent-mo),
//! [Embellished operators](https://w3c.github.io/mathml-core/#embellished-operators),
//! and [glyph stretching](https://w3c.github.io/mathml-core/#algorithms-for-glyph-stretching).

use std::sync::atomic::{AtomicU8, Ordering};

use app_units::Au;
use euclid::num::Zero;
use fonts::MathStretchDirection;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::attr::AttrValue;
use style::context::{SharedStyleContext, TreeCountingCaches};
use style::dom::{DummyElementContext, NodeInfo};
use style::properties::{ComputedValues, StyleBuilder};
use style::rule_cache::RuleCacheConditions;
use style::rule_tree::RuleCascadeFlags;
use style::stylesheets::container_rule::ContainerSizeQuery;
use style::values::computed::font::MathStyle;
use style::values::computed::{Context, LengthPercentage, ToComputedValue};
use web_atoms::{local_name, ns};

use super::operator_dictionary::{
    OperatorDictionaryEntry, OperatorForm, is_inline_stretch_operator, lookup_operator,
};
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
use crate::containing_block::{ConstraintSpace, ContainingBlock};

/// Collect text content and check whether the `<mo>` has ONLY text children.
/// Per MathML Core §3.2.4.1, an `<mo>` has inline stretch axis only if it
/// contains only text content made of a single character `c`.
fn collect_operator_text<'dom>(info: &NodeAndStyleInfo<'dom>) -> (String, bool) {
    let mut text = String::new();
    let mut only_text = true;
    for child in info.node.flat_tree_children() {
        if child.is_text_node() {
            text.push_str(&child.text_content());
        } else {
            only_text = false;
        }
    }
    (text, only_text)
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

fn parse_form_attr<'dom, E: LayoutElement<'dom>>(element: &E) -> Option<OperatorForm> {
    let value = element.attribute_as_str(&ns!(), &local_name!("form"))?;
    if value.eq_ignore_ascii_case("prefix") {
        Some(OperatorForm::Prefix)
    } else if value.eq_ignore_ascii_case("infix") {
        Some(OperatorForm::Infix)
    } else if value.eq_ignore_ascii_case("postfix") {
        Some(OperatorForm::Postfix)
    } else {
        None
    }
}

fn resolve_length_attr<'dom, E: LayoutElement<'dom>>(
    element: &E,
    attr_name: &web_atoms::LocalName,
    context: &SharedStyleContext,
    style: &ServoArc<ComputedValues>,
) -> Option<LengthPercentage> {
    let attr = element.attribute(&ns!(), attr_name)?;
    let specified = match attr {
        AttrValue::LengthPercentage(_, lp) => lp.as_ref()?,
        _ => return None,
    };
    let mut rule_cache_conditions = RuleCacheConditions::default();
    let mut tree_counting_caches = TreeCountingCaches::default();
    let style_builder = StyleBuilder::new(
        context.stylist.device(),
        Some(context.stylist),
        Some(style),
        None,
        None,
        false,
    );
    let computed_context = Context::new(
        style_builder,
        context.quirks_mode(),
        &mut rule_cache_conditions,
        ContainerSizeQuery::none(),
        RuleCascadeFlags::empty(),
        &DummyElementContext,
        &mut tree_counting_caches,
    );
    Some(specified.to_computed_value(&computed_context))
}

const FORM_INFIX: u8 = 0;
const FORM_PREFIX: u8 = 1;
const FORM_POSTFIX: u8 = 2;

fn encode_form(form: OperatorForm) -> u8 {
    match form {
        OperatorForm::Infix => FORM_INFIX,
        OperatorForm::Prefix => FORM_PREFIX,
        OperatorForm::Postfix => FORM_POSTFIX,
    }
}

fn decode_form(val: u8) -> OperatorForm {
    match val {
        FORM_PREFIX => OperatorForm::Prefix,
        FORM_POSTFIX => OperatorForm::Postfix,
        _ => OperatorForm::Infix,
    }
}

/// Core `<mo>` operator box with W3C MathML Core operator dictionary lookup,
/// embellished operator form tracking, `lspace`/`rspace`/`minsize`/`maxsize`,
/// and OpenType MATH glyph stretching.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathOperatorBlock {
    pub base: LayoutBoxBase,
    pub inline_formatting_context: InlineFormattingContext,
    /// Raw text content of `<mo>`.
    content: String,
    /// Single character `c` when `<mo>` contains ONLY text content made of a
    /// single character (per MathML Core §3.2.4.1).
    pub operator_char: Option<char>,
    /// Explicit `form` attribute (`prefix`/`infix`/`postfix`), if present.
    explicit_form: Option<OperatorForm>,
    /// Inferred form from the enclosing grouping element (`MathRowBlock`).
    inferred_form: AtomicU8,
    explicit_stretchy: Option<bool>,
    explicit_symmetric: Option<bool>,
    explicit_largeop: Option<bool>,
    explicit_movablelimits: Option<bool>,
    explicit_accent: Option<bool>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    explicit_lspace: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    explicit_rspace: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    explicit_minsize: Option<LengthPercentage>,
    #[ignore_malloc_size_of = "LengthPercentage does not allocate on the heap"]
    explicit_maxsize: Option<LengthPercentage>,
}

impl MathOperatorBlock {
    pub(crate) fn construct(
        context: &LayoutContext,
        info: &NodeAndStyleInfo,
        inline_formatting_context: InlineFormattingContext,
    ) -> Self {
        let (content, only_text) = collect_operator_text(info);
        let operator_char = if only_text {
            let mut chars = content.chars();
            chars.next().filter(|_| chars.next().is_none())
        } else {
            None
        };

        let (
            explicit_form,
            explicit_stretchy,
            explicit_symmetric,
            explicit_largeop,
            explicit_movablelimits,
            explicit_accent,
            explicit_lspace,
            explicit_rspace,
            explicit_minsize,
            explicit_maxsize,
        ) = if let Some(element) = info.node.as_element() {
            (
                parse_form_attr(&element),
                parse_bool_attr(&element, &local_name!("stretchy")),
                parse_bool_attr(&element, &local_name!("symmetric")),
                parse_bool_attr(&element, &local_name!("largeop")),
                parse_bool_attr(&element, &local_name!("movablelimits")),
                parse_bool_attr(&element, &local_name!("accent")),
                resolve_length_attr(
                    &element,
                    &local_name!("lspace"),
                    &context.style_context,
                    &info.style,
                ),
                resolve_length_attr(
                    &element,
                    &local_name!("rspace"),
                    &context.style_context,
                    &info.style,
                ),
                resolve_length_attr(
                    &element,
                    &local_name!("minsize"),
                    &context.style_context,
                    &info.style,
                ),
                resolve_length_attr(
                    &element,
                    &local_name!("maxsize"),
                    &context.style_context,
                    &info.style,
                ),
            )
        } else {
            (None, None, None, None, None, None, None, None, None, None)
        };

        Self {
            base: LayoutBoxBase::new(info.into(), info.style.clone()),
            inline_formatting_context,
            content,
            operator_char,
            explicit_form,
            inferred_form: AtomicU8::new(FORM_INFIX),
            explicit_stretchy,
            explicit_symmetric,
            explicit_largeop,
            explicit_movablelimits,
            explicit_accent,
            explicit_lspace,
            explicit_rspace,
            explicit_minsize,
            explicit_maxsize,
        }
    }

    /// Set the form inferred from this embellished operator's position inside
    /// its enclosing grouping element (MathML Core §3.2.4.2).
    pub(crate) fn set_inferred_form(&self, form: OperatorForm) {
        self.inferred_form.store(encode_form(form), Ordering::Relaxed);
    }

    fn effective_form(&self) -> OperatorForm {
        self.explicit_form
            .unwrap_or_else(|| decode_form(self.inferred_form.load(Ordering::Relaxed)))
    }

    fn dictionary_entry(&self) -> OperatorDictionaryEntry {
        lookup_operator(&self.content, self.effective_form())
    }

    fn is_symmetric(&self) -> bool {
        self.explicit_symmetric
            .unwrap_or_else(|| self.dictionary_entry().symmetric)
    }

    pub(crate) fn operator_properties(&self) -> MathOperatorProperties {
        let dict = self.dictionary_entry();
        let font_size = Au::from_f32_px(self.base.style.get_font().font_size.computed_size().px());
        let lspace = self
            .explicit_lspace
            .as_ref()
            .map(|lp| lp.to_used_value(font_size).max(Au::zero()))
            .unwrap_or_else(|| OperatorDictionaryEntry::space_to_au(dict.lspace_eighteenths, font_size));
        let rspace = self
            .explicit_rspace
            .as_ref()
            .map(|lp| lp.to_used_value(font_size).max(Au::zero()))
            .unwrap_or_else(|| OperatorDictionaryEntry::space_to_au(dict.rspace_eighteenths, font_size));

        let stretch_axis = if self
            .operator_char
            .is_some_and(is_inline_stretch_operator)
        {
            MathStretchAxis::Inline
        } else {
            MathStretchAxis::Block
        };

        MathOperatorProperties {
            stretchy: self.explicit_stretchy.unwrap_or(dict.stretchy)
                && self.operator_char.is_some(),
            stretch_axis,
            largeop: self.explicit_largeop.unwrap_or(dict.largeop),
            movablelimits: self.explicit_movablelimits.unwrap_or(dict.movablelimits),
            accent: self.explicit_accent.unwrap_or(dict.accent),
            lspace,
            rspace,
        }
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base
            .cached_inline_content_sizes_with(constraint_space, || {
                let mut result = self
                    .inline_formatting_context
                    .compute_inline_content_sizes(layout_context, constraint_space);
                let props = self.operator_properties();
                if props.largeop
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
                            .filter(|&h| h > Au::zero())
                            .unwrap_or_else(|| {
                                Au::from_f32_px(
                                    self.base.style.get_font().font_size.computed_size().px()
                                        * 1.414,
                                )
                            });
                        if let Some(stretched) =
                            font.math_stretch_glyph(ch, MathStretchDirection::Vertical, min_height)
                        {
                            let pbm = LayoutStyle::Default(&self.base.style)
                                .content_box_sizes_and_padding_border_margin(
                                    &constraint_space.into(),
                                )
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
        let is_symmetric = self.is_symmetric();

        if let Some(ch) = self.operator_char {
            let font_group = layout_context.font_context.font_group(style.clone_font());
            if let Some(font) = font_group.first(&layout_context.font_context) {
                let axis = font
                    .math_constants(is_display_style)
                    .map(|c| c.axis_height)
                    .unwrap_or_else(|| font.metrics().x_height / 2);

                let natural_span = |dir: MathStretchDirection| -> Au {
                    font.math_glyph_extents(ch)
                        .map(|ext| match dir {
                            MathStretchDirection::Horizontal => ext.advance.max(Au::zero()),
                            MathStretchDirection::Vertical => {
                                (ext.ascent + ext.descent).max(Au::zero())
                            },
                        })
                        .unwrap_or_else(|| {
                            Au::from_f32_px(style.get_font().font_size.computed_size().px())
                        })
                };

                let clamp_min_max = |mut span: Au, dir: MathStretchDirection| -> Au {
                    let base_span = natural_span(dir);
                    let min_size = self
                        .explicit_minsize
                        .as_ref()
                        .map(|lp| lp.to_used_value(base_span).max(Au::zero()))
                        .unwrap_or(Au::zero());
                    span = span.max(min_size);
                    if let Some(ref max_lp) = self.explicit_maxsize {
                        let max_size = max_lp.to_used_value(base_span).max(min_size);
                        span = span.min(max_size);
                    }
                    span
                };

                let stretch_request: Option<(MathStretchDirection, Au, Option<(Au, Au)>)> =
                    if props.largeop && is_display_style {
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
                                if is_symmetric {
                                    let half = (ascent - axis).max(descent + axis);
                                    ascent = half + axis;
                                    descent = half - axis;
                                }
                                let total = clamp_min_max(
                                    (ascent + descent).max(Au::zero()),
                                    MathStretchDirection::Vertical,
                                );
                                (total > Au::zero()).then_some((
                                    MathStretchDirection::Vertical,
                                    total,
                                    Some((ascent, descent)),
                                ))
                            },
                            (MathStretchAxis::Inline, Some(MathStretchTarget::Inline(width))) => {
                                let clamped =
                                    clamp_min_max(width, MathStretchDirection::Horizontal);
                                (clamped > Au::zero()).then_some((
                                    MathStretchDirection::Horizontal,
                                    clamped,
                                    None,
                                ))
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
                            if props.largeop && is_display_style {
                                let half = glyph_height / 2;
                                let ascent = half + axis;
                                let descent = glyph_height - ascent;
                                (ascent, descent, stretched.ascent)
                            } else if let Some((req_ascent, req_descent)) = block_target {
                                if is_symmetric {
                                    let half = glyph_height / 2;
                                    let ascent = half + axis;
                                    let descent = glyph_height - ascent;
                                    (ascent, descent, stretched.ascent)
                                } else {
                                    let ascent = req_ascent.max(stretched.ascent);
                                    let descent = (glyph_height - ascent).max(req_descent);
                                    let y_center_shift = ((ascent + descent) - glyph_height) / 2;
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

        let fragments = result.fragments.into_physical_fragments(
            style.writing_mode,
            crate::geom::LogicalVec2 {
                inline: containing_block.inline_size(),
                block: result.content_block_size,
            },
        );
        let mut natural_inline_size = Au::zero();
        for frag in &fragments {
            if let Some(base) = frag.base() {
                let rect = base.rect();
                natural_inline_size = natural_inline_size.max(rect.origin.x + rect.size.width);
            }
        }
        if natural_inline_size == Au::zero() {
            natural_inline_size = containing_block.inline_size();
        }
        let natural_baseline = result.baselines.first.unwrap_or(result.content_block_size);
        let mut metrics = MathBoxMetrics::new(
            natural_inline_size,
            result.content_block_size,
            natural_baseline,
        );

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

    pub(crate) fn attached_to_tree(&self, _layout_box: WeakLayoutBox) {}

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        if let Some(element) = node.as_element()
            && element.local_name() == &local_name!("mo")
        {
            let info = NodeAndStyleInfo::new(*node, new_style.clone());
            let (content, only_text) = collect_operator_text(&info);
            self.operator_char = if only_text {
                let mut chars = content.chars();
                chars.next().filter(|_| chars.next().is_none())
            } else {
                None
            };
            self.content = content;
            self.explicit_form = parse_form_attr(&element);
            self.explicit_stretchy = parse_bool_attr(&element, &local_name!("stretchy"));
            self.explicit_symmetric = parse_bool_attr(&element, &local_name!("symmetric"));
            self.explicit_largeop = parse_bool_attr(&element, &local_name!("largeop"));
            self.explicit_movablelimits = parse_bool_attr(&element, &local_name!("movablelimits"));
            self.explicit_accent = parse_bool_attr(&element, &local_name!("accent"));
            self.explicit_lspace =
                resolve_length_attr(&element, &local_name!("lspace"), context, new_style);
            self.explicit_rspace =
                resolve_length_attr(&element, &local_name!("rspace"), context, new_style);
            self.explicit_minsize =
                resolve_length_attr(&element, &local_name!("minsize"), context, new_style);
            self.explicit_maxsize =
                resolve_length_attr(&element, &local_name!("maxsize"), context, new_style);
        }
        self.inline_formatting_context
            .repair_style(context, node, new_style);
    }
}
