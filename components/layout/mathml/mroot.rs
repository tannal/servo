/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Square root (`<msqrt>`) and indexed radical (`<mroot>`) layout.
//!
//! Implements [MathML Core §3.3.3 Radicals](https://w3c.github.io/mathml-core/#radicals-msqrt-mroot)
//! using OpenType MATH radical constants and stretched U+221A (`√`) glyph variants/assemblies.

use app_units::Au;
use fonts::{Font, MathStretchDirection};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::Zero;
use style::context::SharedStyleContext;
use style::logical_geometry::LogicalAxis;
use style::properties::ComputedValues;
use style::values::computed::font::MathStyle;

use super::{MathLevelBox, MathRowBlock};
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::fragment_tree::{MathGlyphRun, MathPaintInfo};
use crate::geom::{PhysicalPoint, PhysicalRect, PhysicalSize};
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutCache, MathLayoutResult, MathStretchTarget, translate_fragments,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult, Size};
use crate::style_ext::LayoutStyle;
use crate::containing_block::{ConstraintSpace, ContainingBlock};

const RADICAL_SIGN: char = '\u{221A}';

#[derive(Clone, Copy, Debug)]
struct RadicalParameters {
    vertical_gap: Au,
    rule_thickness: Au,
    extra_ascender: Au,
    kern_before_degree: Au,
    kern_after_degree: Au,
    degree_bottom_raise_percent: i32,
}

impl RadicalParameters {
    fn new(font: Option<&Font>, font_size: Au, is_display_style: bool) -> Self {
        if let Some(c) = font.and_then(|f| f.math_constants(is_display_style)) {
            return Self {
                vertical_gap: c.radical_vertical_gap,
                rule_thickness: c.radical_rule_thickness,
                extra_ascender: c.radical_extra_ascender,
                kern_before_degree: c.radical_kern_before_degree,
                kern_after_degree: c.radical_kern_after_degree,
                degree_bottom_raise_percent: c.radical_degree_bottom_raise_percent,
            };
        }

        // MathML Core §3.3.3 fallback constants when font has no MATH table.
        let (rule_thickness, x_height) = font
            .map(|f| {
                let m = f.metrics();
                (
                    m.underline_size.max(Au::from_px(1)),
                    m.x_height.max(Au::zero()),
                )
            })
            .unwrap_or((Au::from_px(1), font_size / 2));

        let vertical_gap = if is_display_style {
            rule_thickness + x_height / 4
        } else {
            rule_thickness + rule_thickness / 4
        };

        Self {
            vertical_gap,
            rule_thickness,
            extra_ascender: rule_thickness,
            kern_before_degree: Au((font_size.0 as f64 * 5.0 / 18.0) as i32),
            kern_after_degree: Au((-font_size.0 as f64 * 10.0 / 18.0) as i32),
            degree_bottom_raise_percent: 60,
        }
    }
}

/// Box-tree node for `<msqrt>` and `<mroot>`.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathRadicalBlock {
    /// Owns styled base, all children for `<msqrt>`, or out-of-flow/fallback children for `<mroot>`.
    pub inner_row: MathRowBlock,
    /// `true` for `<mroot>`, `false` for `<msqrt>`.
    pub is_mroot: bool,
    /// First in-flow child when `is_mroot` has valid arity (2).
    pub base_child: Option<ArcRefCell<MathLevelBox>>,
    /// Second in-flow child (degree/index) when `is_mroot` has valid arity (2).
    pub index_child: Option<ArcRefCell<MathLevelBox>>,
    /// Incremental layout result cache.
    layout_cache: MathLayoutCache,
}

impl MathRadicalBlock {
    pub(crate) fn construct(
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
        is_mroot: bool,
    ) -> Self {
        if !is_mroot {
            return Self {
                inner_row: MathRowBlock::construct(node_and_style_info, children),
                is_mroot: false,
                base_child: None,
                index_child: None,
                layout_cache: MathLayoutCache::default(),
            };
        }

        let in_flow_indices: Vec<_> = children
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.borrow().is_in_flow().then_some(i))
            .collect();

        if in_flow_indices.len() != 2 {
            // Invalid mroot arity falls back to mrow layout.
            return Self {
                inner_row: MathRowBlock::construct(node_and_style_info, children),
                is_mroot: true,
                base_child: None,
                index_child: None,
                layout_cache: MathLayoutCache::default(),
            };
        }

        let base_idx = in_flow_indices[0];
        let index_idx = in_flow_indices[1];
        let mut base_child = None;
        let mut index_child = None;
        let mut out_of_flow = Vec::new();
        for (i, child) in children.into_iter().enumerate() {
            if i == base_idx {
                base_child = Some(child);
            } else if i == index_idx {
                index_child = Some(child);
            } else {
                out_of_flow.push(child);
            }
        }

        Self {
            inner_row: MathRowBlock::construct(node_and_style_info, out_of_flow),
            is_mroot: true,
            base_child,
            index_child,
            layout_cache: MathLayoutCache::default(),
        }
    }

    pub(crate) fn base(&self) -> &LayoutBoxBase {
        &self.inner_row.base
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.is_mroot || (self.base_child.is_some() && self.index_child.is_some())
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.inner_row.repair_style(context, node, new_style);
        self.layout_cache.clear();
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        if let Some(ref base) = self.base_child {
            base.borrow().attached_to_tree(layout_box.clone());
        }
        if let Some(ref index) = self.index_child {
            index.borrow().attached_to_tree(layout_box.clone());
        }
        self.inner_row.attached_to_tree(layout_box);
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base().cached_inline_content_sizes_with(constraint_space, || {
            if !self.is_valid() {
                return self
                    .inner_row
                    .inline_content_sizes(layout_context, constraint_space);
            }

            let style = &self.base().style;
            let is_display = style.get_font().math_style == MathStyle::Normal;
            let font_size = Au::from_f32_px(style.get_font().font_size.computed_size().px());
            let font_group = layout_context.font_context.font_group(style.clone_font());
            let font = font_group.first(&layout_context.font_context);
            let params = RadicalParameters::new(
                font.as_ref().map(|f| &***f),
                font_size,
                is_display,
            );

            let radical_width = font
                .as_ref()
                .and_then(|f| f.math_glyph_extents(RADICAL_SIGN))
                .map(|ext| ext.advance.max(Au::zero()))
                .unwrap_or(font_size / 2);

            let (base_sizes, index_extra_min, index_extra_max, index_dep) = if self.is_mroot {
                let base_s = self
                    .base_child
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .inline_content_sizes(layout_context, constraint_space);
                let idx_s = self
                    .index_child
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .inline_content_sizes(layout_context, constraint_space);
                let kern = params.kern_before_degree + params.kern_after_degree;
                (
                    base_s,
                    (idx_s.sizes.min_content + kern).max(Au::zero()),
                    (idx_s.sizes.max_content + kern).max(Au::zero()),
                    idx_s.depends_on_block_constraints,
                )
            } else {
                let base_s = self
                    .inner_row
                    .content_inline_sizes(layout_context, constraint_space);
                (base_s, Au::zero(), Au::zero(), false)
            };

            let pbm = LayoutStyle::Default(style)
                .content_box_sizes_and_padding_border_margin(&constraint_space.into())
                .pbm;
            let pbm_inline =
                pbm.padding_border_sums.inline + pbm.margin.auto_is(Au::zero).inline_sum();

            InlineContentSizesResult {
                sizes: ContentSizes {
                    min_content: index_extra_min
                        + radical_width
                        + base_sizes.sizes.min_content
                        + pbm_inline,
                    max_content: index_extra_max
                        + radical_width
                        + base_sizes.sizes.max_content
                        + pbm_inline,
                },
                depends_on_block_constraints: base_sizes.depends_on_block_constraints || index_dep,
            }
        })
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        if let Some(cached) = self.layout_cache.get(
            self.base(),
            containing_block,
            target_size,
            positioning_context,
        ) {
            return cached;
        }

        if !self.is_valid() {
            return self.inner_row.layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            );
        }

        let mut local_positioning = PositioningContext::default();
        let style = &self.base().style;
        let is_rtl = !style.writing_mode.is_bidi_ltr();
        let is_display = style.get_font().math_style == MathStyle::Normal;
        let font_size = Au::from_f32_px(style.get_font().font_size.computed_size().px());

        let layout_style = LayoutStyle::Default(style);
        let box_sizes =
            layout_style.content_box_sizes_and_padding_border_margin(&containing_block.into());
        let pbm = box_sizes.pbm;

        // 1. Lay out base (and index for mroot).
        let (mut base_output, mut index_output) = if self.is_mroot {
            let base_out = self
                .base_child
                .as_ref()
                .unwrap()
                .borrow()
                .layout_for_math_parent(
                    layout_context,
                    &mut local_positioning,
                    containing_block,
                    style,
                    None,
                );
            let idx_out = self
                .index_child
                .as_ref()
                .unwrap()
                .borrow()
                .layout_for_math_parent(
                    layout_context,
                    &mut local_positioning,
                    containing_block,
                    style,
                    None,
                );
            (base_out, Some(idx_out))
        } else {
            let base_out = self.inner_row.layout_contents(
                layout_context,
                &mut local_positioning,
                containing_block,
                None,
            );
            (base_out, None)
        };

        // 2. Resolve OpenType MATH radical parameters and stretch U+221A (√).
        let font_group = layout_context.font_context.font_group(style.clone_font());
        let font = font_group.first(&layout_context.font_context);
        let params = RadicalParameters::new(
            font.as_ref().map(|f| &***f),
            font_size,
            is_display,
        );

        let base_ascent = base_output.metrics.ascent();
        let base_descent = base_output.metrics.descent();
        let base_width = base_output.metrics.size.inline;

        let target_radical_height =
            base_ascent + base_descent + params.vertical_gap + params.rule_thickness;
        let stretched_surd = font.as_ref().and_then(|f| {
            f.math_stretch_glyph(
                RADICAL_SIGN,
                MathStretchDirection::Vertical,
                target_radical_height,
            )
        });

        let (surd_width, surd_ascent, surd_descent) = stretched_surd
            .as_ref()
            .map(|s| (s.inline_size, s.ascent, s.descent))
            .unwrap_or((font_size / 2, base_ascent + params.vertical_gap, base_descent));
        let surd_height = (surd_ascent + surd_descent).max(Au::zero());

        // Distribute excess radical height into vertical clearance per MathML Core §3.3.3.
        let mut clearance = params.vertical_gap;
        if surd_height > target_radical_height {
            clearance += (surd_height - target_radical_height) / 2;
        }

        // 3. Compute overall ascent and descent relative to the base's alphabetic baseline.
        let overbar_top_above_baseline = base_ascent + clearance + params.rule_thickness;
        let radical_ascent = overbar_top_above_baseline + params.extra_ascender;
        let radical_descent = base_descent
            .max(surd_height - overbar_top_above_baseline)
            .max(Au::zero());

        let (mut total_ascent, mut total_descent) = (radical_ascent, radical_descent);

        // Compute degree/index vertical shift for mroot.
        let index_shift_up = if let Some(ref idx) = index_output {
            let raise =
                Au((surd_height.0 as i64 * params.degree_bottom_raise_percent as i64 / 100) as i32);
            // Distance from bottom of radical sign up to the index's bottom edge.
            let radical_bottom_below_baseline = radical_descent;
            let shift_up = raise - radical_bottom_below_baseline + idx.metrics.descent();
            total_ascent = total_ascent.max(shift_up + idx.metrics.ascent());
            total_descent = total_descent.max(idx.metrics.descent() - shift_up);
            shift_up
        } else {
            Au::zero()
        };

        // 4. Horizontal positions (supporting LTR and RTL).
        let index_width = index_output
            .as_ref()
            .map_or(Au::zero(), |idx| idx.metrics.size.inline);
        let index_slot_width = if index_output.is_some() {
            (params.kern_before_degree + index_width + params.kern_after_degree).max(Au::zero())
        } else {
            Au::zero()
        };

        let natural_width = index_slot_width + surd_width + base_width;
        let content_width = box_sizes.content_box_sizes.inline.resolve(
            LogicalAxis::Inline,
            Size::FitContent,
            Au::zero,
            Some(containing_block.inline_size()),
            || natural_width.into(),
            false,
        );
        let natural_block = total_ascent + total_descent;
        let content_block = box_sizes.content_box_sizes.block.resolve(
            LogicalAxis::Block,
            Size::FitContent,
            Au::zero,
            containing_block.block_size(),
            || natural_block.into(),
            false,
        );

        let (index_x, surd_x, base_x) = if is_rtl {
            let base_x = Au::zero();
            let surd_x = base_width;
            let index_x = surd_x + surd_width + params.kern_after_degree;
            (index_x, surd_x, base_x)
        } else {
            let index_x = params.kern_before_degree;
            let surd_x = index_slot_width;
            let base_x = index_slot_width + surd_width;
            (index_x, surd_x, base_x)
        };

        // 5. Translate child fragments.
        let base_y = total_ascent - base_ascent;
        translate_fragments(&mut base_output.fragments, base_x, base_y);

        let mut children = Vec::with_capacity(
            base_output.fragments.len()
                + index_output.as_ref().map_or(0, |i| i.fragments.len())
                + if self.is_mroot {
                    self.inner_row.children.len()
                } else {
                    0
                },
        );
        if let Some(mut idx) = index_output.take() {
            let idx_y = total_ascent - index_shift_up - idx.metrics.ascent();
            translate_fragments(&mut idx.fragments, index_x, idx_y);
            children.extend(idx.fragments);
        }
        children.extend(base_output.fragments);

        // Out-of-flow children in valid mroot.
        if self.is_mroot {
            for oof_child in &self.inner_row.children {
                let oof_out = oof_child.borrow().layout_for_math_parent(
                    layout_context,
                    &mut local_positioning,
                    containing_block,
                    style,
                    None,
                );
                children.extend(oof_out.fragments);
            }
        }

        // 6. Build overbar rule and stretched surd paint payload.
        let overbar_y = total_ascent - overbar_top_above_baseline;
        let rule_rect = PhysicalRect::new(
            PhysicalPoint::new(base_x, overbar_y),
            PhysicalSize::new(base_width, params.rule_thickness),
        );

        let radical_glyph = stretched_surd.zip(font.as_ref()).map(|(surd, f)| {
            // Place top of surd ink aligned with overbar_y.
            let glyph_baseline_y = overbar_y + surd.ascent;
            MathGlyphRun {
                font_key: f.key(layout_context.painter_id, &layout_context.font_context),
                glyphs: surd.glyphs,
                origin: PhysicalPoint::new(surd_x, glyph_baseline_y),
                bounds: PhysicalRect::new(
                    PhysicalPoint::new(surd_x, overbar_y),
                    PhysicalSize::new(surd.inline_size, surd_height),
                ),
            }
        });

        let result = MathLayoutResult::from_content(
            self.base(),
            containing_block,
            &pbm,
            children,
            MathBoxMetrics::new(content_width, content_block, total_ascent),
            Some(MathPaintInfo::Radical {
                rule_rect,
                radical_glyph,
            }),
        );
        self.layout_cache.store(
            self.base(),
            containing_block,
            target_size,
            &result,
            &local_positioning,
        );
        positioning_context.append(local_positioning);
        result
    }
}
