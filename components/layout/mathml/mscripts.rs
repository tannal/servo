/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Subscript (`<msub>`), superscript (`<msup>`), sub-superscript (`<msubsup>`),
//! and multiscripts (`<mmultiscripts>` with `<mprescripts>`) layout.
//!
//! Implements [MathML Core §3.4.1–§3.4.3](https://w3c.github.io/mathml-core/#scripts-and-limits).

use app_units::Au;
use fonts::Font;
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
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutCache, MathLayoutResult, MathOperatorProperties, MathStretchTarget,
    translate_fragments,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult, Size};
use crate::style_ext::LayoutStyle;
use crate::containing_block::{ConstraintSpace, ContainingBlock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, MallocSizeOf)]
pub(crate) enum MathScriptsKind {
    Sub,
    Sup,
    SubSup,
    MultiScripts,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScriptParameters {
    pub subscript_shift_down: Au,
    pub subscript_top_max: Au,
    pub subscript_baseline_drop_min: Au,
    pub superscript_shift_up: Au,
    pub superscript_bottom_min: Au,
    pub superscript_baseline_drop_max: Au,
    pub sub_superscript_gap_min: Au,
    pub superscript_bottom_max_with_subscript: Au,
    pub space_after_script: Au,
}

impl ScriptParameters {
    pub(crate) fn new(
        font: Option<&Font>,
        font_size: Au,
        is_display_style: bool,
        is_cramped: bool,
    ) -> Self {
        if let Some(c) = font.and_then(|f| f.math_constants(is_display_style)) {
            return Self {
                subscript_shift_down: c.subscript_shift_down,
                subscript_top_max: c.subscript_top_max,
                subscript_baseline_drop_min: c.subscript_baseline_drop_min,
                superscript_shift_up: if is_cramped {
                    c.superscript_shift_up_cramped
                } else {
                    c.superscript_shift_up
                },
                superscript_bottom_min: c.superscript_bottom_min,
                superscript_baseline_drop_max: c.superscript_baseline_drop_max,
                sub_superscript_gap_min: c.sub_superscript_gap_min,
                superscript_bottom_max_with_subscript: c.superscript_bottom_max_with_subscript,
                space_after_script: c.space_after_script,
            };
        }

        // MathML Core fallback constants when font lacks an OpenType MATH table.
        let (x_height, rule_thickness) = font
            .map(|f| {
                let m = f.metrics();
                (
                    m.x_height.max(font_size / 2),
                    m.underline_size.max(Au::from_px(1)),
                )
            })
            .unwrap_or((font_size / 2, Au::from_px(1)));

        Self {
            subscript_shift_down: x_height / 3,
            subscript_top_max: (x_height * 4) / 5,
            subscript_baseline_drop_min: Au::zero(),
            superscript_shift_up: if is_cramped {
                (x_height * 3) / 4
            } else {
                x_height
            },
            superscript_bottom_min: x_height / 4,
            superscript_baseline_drop_max: Au::zero(),
            sub_superscript_gap_min: rule_thickness * 4,
            superscript_bottom_max_with_subscript: (x_height * 4) / 5,
            space_after_script: (font_size / 24).max(Au::from_px(1) / 2),
        }
    }

    /// Compute vertical shifts `(sub_shift_down, sup_shift_up)` for an optional
    /// `(sub_metrics, sup_metrics)` pair attached to a base with `(base_ascent, base_descent)`.
    pub(crate) fn compute_shifts(
        &self,
        base_ascent: Au,
        base_descent: Au,
        sub: Option<&MathBoxMetrics>,
        sup: Option<&MathBoxMetrics>,
    ) -> (Au, Au) {
        let mut sub_shift = sub.map_or(Au::zero(), |s| {
            self.subscript_shift_down
                .max(s.ascent() - self.subscript_top_max)
                .max(base_descent + self.subscript_baseline_drop_min)
        });

        let mut sup_shift = sup.map_or(Au::zero(), |s| {
            self.superscript_shift_up
                .max(self.superscript_bottom_min + s.descent())
                .max(base_ascent - self.superscript_baseline_drop_max)
        });

        if let (Some(sub_m), Some(sup_m)) = (sub, sup) {
            let sub_top = sub_m.ascent() - sub_shift;
            let sup_bottom = sup_shift - sup_m.descent();
            let gap = sup_bottom - sub_top;
            if gap < self.sub_superscript_gap_min {
                let target_sup_bottom =
                    (sub_top + self.sub_superscript_gap_min).min(self.superscript_bottom_max_with_subscript);
                if target_sup_bottom > sup_bottom {
                    sup_shift += target_sup_bottom - sup_bottom;
                }
                let new_gap = (sup_shift - sup_m.descent()) - (sub_m.ascent() - sub_shift);
                if new_gap < self.sub_superscript_gap_min {
                    sub_shift += self.sub_superscript_gap_min - new_gap;
                }
            }
        }

        (sub_shift, sup_shift)
    }
}

/// A `(subscript, superscript)` pair in `msub`, `msup`, `msubsup`, or `mmultiscripts`.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct ScriptPair {
    pub sub: Option<ArcRefCell<MathLevelBox>>,
    pub sup: Option<ArcRefCell<MathLevelBox>>,
}

/// Box-tree node for `<msub>`, `<msup>`, `<msubsup>`, and `<mmultiscripts>`.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathScriptsBlock {
    /// Owns styled base and either out-of-flow children (when valid) or all children (fallback).
    pub inner_row: MathRowBlock,
    pub kind: MathScriptsKind,
    pub base_child: Option<ArcRefCell<MathLevelBox>>,
    pub postscripts: Vec<ScriptPair>,
    pub prescripts: Vec<ScriptPair>,
    layout_cache: MathLayoutCache,
}

impl MathScriptsBlock {
    pub(crate) fn construct(
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
        kind: MathScriptsKind,
    ) -> Self {
        let mut in_flow = Vec::new();
        let mut out_of_flow = Vec::new();
        for (i, child) in children.iter().enumerate() {
            if child.borrow().is_in_flow() {
                in_flow.push(i);
            } else {
                out_of_flow.push(i);
            }
        }

        let valid = match kind {
            MathScriptsKind::Sub | MathScriptsKind::Sup => in_flow.len() == 2,
            MathScriptsKind::SubSup => in_flow.len() == 3,
            MathScriptsKind::MultiScripts => {
                if in_flow.is_empty() {
                    false
                } else {
                    let mut prescripts_pos = None;
                    let mut multiple_prescripts = false;
                    for (pos, &idx) in in_flow.iter().enumerate().skip(1) {
                        let is_mprescripts = children[idx].borrow().is_mprescripts();
                        if is_mprescripts {
                            if prescripts_pos.is_some() {
                                multiple_prescripts = true;
                                break;
                            }
                            prescripts_pos = Some(pos);
                        }
                    }
                    if multiple_prescripts {
                        false
                    } else if let Some(p) = prescripts_pos {
                        let post_count = p - 1;
                        let pre_count = in_flow.len() - p - 1;
                        post_count % 2 == 0 && pre_count % 2 == 0
                    } else {
                        (in_flow.len() - 1) % 2 == 0
                    }
                }
            },
        };

        if !valid {
            return Self {
                inner_row: MathRowBlock::construct(node_and_style_info, children),
                kind,
                base_child: None,
                postscripts: Vec::new(),
                prescripts: Vec::new(),
                layout_cache: MathLayoutCache::default(),
            };
        }

        let mut slots: Vec<Option<ArcRefCell<MathLevelBox>>> =
            children.into_iter().map(Some).collect();
        let take = |slots: &mut [Option<ArcRefCell<MathLevelBox>>], idx: usize| {
            slots[idx].take().unwrap()
        };

        let base_child = Some(take(&mut slots, in_flow[0]));
        let mut postscripts = Vec::new();
        let mut prescripts = Vec::new();

        match kind {
            MathScriptsKind::Sub => {
                postscripts.push(ScriptPair {
                    sub: Some(take(&mut slots, in_flow[1])),
                    sup: None,
                });
            },
            MathScriptsKind::Sup => {
                postscripts.push(ScriptPair {
                    sub: None,
                    sup: Some(take(&mut slots, in_flow[1])),
                });
            },
            MathScriptsKind::SubSup => {
                postscripts.push(ScriptPair {
                    sub: Some(take(&mut slots, in_flow[1])),
                    sup: Some(take(&mut slots, in_flow[2])),
                });
            },
            MathScriptsKind::MultiScripts => {
                let mut i = 1;
                let mut in_pre = false;
                while i < in_flow.len() {
                    if slots[in_flow[i]]
                        .as_ref()
                        .is_some_and(|c| c.borrow().is_mprescripts())
                    {
                        in_pre = true;
                        i += 1;
                        continue;
                    }
                    let sub = take(&mut slots, in_flow[i]);
                    let sup = take(&mut slots, in_flow[i + 1]);
                    let pair = ScriptPair {
                        sub: Some(sub),
                        sup: Some(sup),
                    };
                    if in_pre {
                        prescripts.push(pair);
                    } else {
                        postscripts.push(pair);
                    }
                    i += 2;
                }
            },
        }

        let oof_children: Vec<_> = out_of_flow
            .into_iter()
            .filter_map(|idx| slots[idx].take())
            .collect();

        Self {
            inner_row: MathRowBlock::construct(node_and_style_info, oof_children),
            kind,
            base_child,
            postscripts,
            prescripts,
            layout_cache: MathLayoutCache::default(),
        }
    }

    pub(crate) fn base(&self) -> &LayoutBoxBase {
        &self.inner_row.base
    }

    pub(crate) fn base_mut(&mut self) -> &mut LayoutBoxBase {
        &mut self.inner_row.base
    }

    pub(crate) fn operator_properties(&self) -> Option<MathOperatorProperties> {
        if let Some(ref base) = self.base_child {
            return base.borrow().operator_properties();
        }
        self.inner_row.operator_properties()
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
        for pair in self.prescripts.iter().chain(self.postscripts.iter()) {
            if let Some(ref sub) = pair.sub {
                sub.borrow().attached_to_tree(layout_box.clone());
            }
            if let Some(ref sup) = pair.sup {
                sup.borrow().attached_to_tree(layout_box.clone());
            }
        }
        self.inner_row.attached_to_tree(layout_box);
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base().cached_inline_content_sizes_with(constraint_space, || {
            let Some(ref base_child) = self.base_child else {
                return self
                    .inner_row
                    .inline_content_sizes(layout_context, constraint_space);
            };

            let style = &self.base().style;
            let is_display = style.get_font().math_style == MathStyle::Normal;
            let is_cramped = false;
            let font_size = Au::from_f32_px(style.get_font().font_size.computed_size().px());
            let font_group = layout_context.font_context.font_group(style.clone_font());
            let font = font_group.first(&layout_context.font_context);
            let params = ScriptParameters::new(
                font.as_ref().map(|f| &***f),
                font_size,
                is_display,
                is_cramped,
            );

            let base_s = base_child
                .borrow()
                .inline_content_sizes(layout_context, constraint_space);
            let mut min_w = base_s.sizes.min_content;
            let mut max_w = base_s.sizes.max_content;
            let mut dep = base_s.depends_on_block_constraints;

            let mut add_pairs = |pairs: &[ScriptPair]| {
                if !pairs.is_empty() {
                    min_w += params.space_after_script;
                    max_w += params.space_after_script;
                }
                for pair in pairs {
                    let (sub_min, sub_max, sub_dep) = pair
                        .sub
                        .as_ref()
                        .map(|s| {
                            let r = s
                                .borrow()
                                .inline_content_sizes(layout_context, constraint_space);
                            (r.sizes.min_content, r.sizes.max_content, r.depends_on_block_constraints)
                        })
                        .unwrap_or((Au::zero(), Au::zero(), false));
                    let (sup_min, sup_max, sup_dep) = pair
                        .sup
                        .as_ref()
                        .map(|s| {
                            let r = s
                                .borrow()
                                .inline_content_sizes(layout_context, constraint_space);
                            (r.sizes.min_content, r.sizes.max_content, r.depends_on_block_constraints)
                        })
                        .unwrap_or((Au::zero(), Au::zero(), false));
                    min_w += sub_min.max(sup_min);
                    max_w += sub_max.max(sup_max);
                    dep |= sub_dep || sup_dep;
                }
            };

            add_pairs(&self.prescripts);
            add_pairs(&self.postscripts);

            let pbm = LayoutStyle::Default(style)
                .content_box_sizes_and_padding_border_margin(&constraint_space.into())
                .pbm;
            let extra =
                pbm.padding_border_sums.inline + pbm.margin.auto_is(Au::zero).inline_sum();

            InlineContentSizesResult {
                sizes: ContentSizes {
                    min_content: min_w + extra,
                    max_content: max_w + extra,
                },
                depends_on_block_constraints: dep,
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

        let Some(ref base_child) = self.base_child else {
            return self.inner_row.layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            );
        };

        let mut local_positioning = PositioningContext::default();
        let style = &self.base().style;
        let is_rtl = !style.writing_mode.is_bidi_ltr();
        let is_display = style.get_font().math_style == MathStyle::Normal;
        let is_cramped = false;
        let font_size = Au::from_f32_px(style.get_font().font_size.computed_size().px());

        let layout_style = LayoutStyle::Default(style);
        let box_sizes =
            layout_style.content_box_sizes_and_padding_border_margin(&containing_block.into());
        let pbm = box_sizes.pbm;

        // Forward stretch target only to an embellished base operator.
        let base_borrow = base_child.borrow();
        let base_stretch = target_size.filter(|_| base_borrow.operator_properties().is_some());
        let mut base_out = base_borrow.layout_for_math_parent(
            layout_context,
            &mut local_positioning,
            containing_block,
            style,
            base_stretch,
        );

        let font_group = layout_context.font_context.font_group(style.clone_font());
        let font = font_group.first(&layout_context.font_context);
        let params = ScriptParameters::new(
            font.as_ref().map(|f| &***f),
            font_size,
            is_display,
            is_cramped,
        );

        // Measure all prescript and postscript pairs.
        let mut measure_pairs = |pairs: &[ScriptPair]| -> Vec<(Option<MathLayoutResult>, Option<MathLayoutResult>)> {
            pairs
                .iter()
                .map(|pair| {
                    let sub_out = pair.sub.as_ref().map(|s| {
                        s.borrow().layout_for_math_parent(
                            layout_context,
                            &mut local_positioning,
                            containing_block,
                            style,
                            None,
                        )
                    });
                    let sup_out = pair.sup.as_ref().map(|s| {
                        s.borrow().layout_for_math_parent(
                            layout_context,
                            &mut local_positioning,
                            containing_block,
                            style,
                            None,
                        )
                    });
                    (sub_out, sup_out)
                })
                .collect()
        };

        let pre_outputs = measure_pairs(&self.prescripts);
        let post_outputs = measure_pairs(&self.postscripts);

        let base_ascent = base_out.metrics.ascent();
        let base_descent = base_out.metrics.descent();
        let base_ic = base_out.metrics.italic_correction;

        // Compute unified shifts across all script pairs (MathML Core §3.4.3).
        let mut max_sub_shift = Au::zero();
        let mut max_sup_shift = Au::zero();
        let mut total_ascent = base_ascent;
        let mut total_descent = base_descent;

        for (sub_out, sup_out) in pre_outputs.iter().chain(post_outputs.iter()) {
            let (sub_shift, sup_shift) = params.compute_shifts(
                base_ascent,
                base_descent,
                sub_out.as_ref().map(|o| &o.metrics),
                sup_out.as_ref().map(|o| &o.metrics),
            );
            max_sub_shift = max_sub_shift.max(sub_shift);
            max_sup_shift = max_sup_shift.max(sup_shift);
        }

        for (sub_out, sup_out) in pre_outputs.iter().chain(post_outputs.iter()) {
            if let Some(sub) = sub_out {
                total_ascent = total_ascent.max(sub.metrics.ascent() - max_sub_shift);
                total_descent = total_descent.max(max_sub_shift + sub.metrics.descent());
            }
            if let Some(sup) = sup_out {
                total_ascent = total_ascent.max(max_sup_shift + sup.metrics.ascent());
                total_descent = total_descent.max(sup.metrics.descent() - max_sup_shift);
            }
        }

        // Compute horizontal widths and place prescripts, base, postscripts.
        let mut pre_width = if pre_outputs.is_empty() {
            Au::zero()
        } else {
            params.space_after_script
        };
        let mut pre_pair_widths = Vec::with_capacity(pre_outputs.len());
        for (sub_out, sup_out) in &pre_outputs {
            let w = sub_out
                .as_ref()
                .map_or(Au::zero(), |o| o.metrics.size.inline)
                .max(sup_out.as_ref().map_or(Au::zero(), |o| o.metrics.size.inline));
            pre_pair_widths.push(w);
            pre_width += w;
        }

        let mut post_width = if post_outputs.is_empty() {
            Au::zero()
        } else {
            params.space_after_script
        };
        let mut post_pair_widths = Vec::with_capacity(post_outputs.len());
        for (i, (sub_out, sup_out)) in post_outputs.iter().enumerate() {
            let sub_w = sub_out
                .as_ref()
                .map_or(Au::zero(), |o| o.metrics.size.inline);
            let sup_w = sup_out
                .as_ref()
                .map_or(Au::zero(), |o| o.metrics.size.inline);
            let effective_sub_w = if i == 0 {
                (sub_w - base_ic).max(Au::zero())
            } else {
                sub_w
            };
            let w = effective_sub_w.max(sup_w);
            post_pair_widths.push(w);
            post_width += w;
        }

        let base_width = base_out.metrics.size.inline;
        let natural_width = pre_width + base_width + post_width;
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

        let mut children = Vec::new();

        // Helper to place a script pair at horizontal slot `[slot_x, slot_x + slot_w]`.
        let place_pair = |children: &mut Vec<_>,
                          mut sub_out: Option<MathLayoutResult>,
                          mut sup_out: Option<MathLayoutResult>,
                          slot_x: Au,
                          slot_w: Au,
                          align_right: bool,
                          sub_ic_adjust: Au| {
            if let Some(mut sub) = sub_out.take() {
                let x = if align_right {
                    slot_x + slot_w - sub.metrics.size.inline
                } else {
                    slot_x - sub_ic_adjust
                };
                let y = total_ascent + max_sub_shift - sub.metrics.ascent();
                translate_fragments(&mut sub.fragments, x, y);
                children.extend(sub.fragments);
            }
            if let Some(mut sup) = sup_out.take() {
                let x = if align_right {
                    slot_x + slot_w - sup.metrics.size.inline
                } else {
                    slot_x
                };
                let y = total_ascent - max_sup_shift - sup.metrics.ascent();
                translate_fragments(&mut sup.fragments, x, y);
                children.extend(sup.fragments);
            }
        };

        if !is_rtl {
            // LTR: [space_after_script + prescripts] [base] [postscripts + space_after_script]
            let mut cursor = if pre_outputs.is_empty() {
                Au::zero()
            } else {
                params.space_after_script
            };
            for ((sub_out, sup_out), &w) in pre_outputs.into_iter().zip(&pre_pair_widths) {
                place_pair(&mut children, sub_out, sup_out, cursor, w, true, Au::zero());
                cursor += w;
            }

            let base_x = cursor;
            translate_fragments(&mut base_out.fragments, base_x, total_ascent - base_ascent);
            children.extend(base_out.fragments);
            cursor += base_width;

            for (i, ((sub_out, sup_out), &w)) in
                post_outputs.into_iter().zip(&post_pair_widths).enumerate()
            {
                let ic = if i == 0 { base_ic } else { Au::zero() };
                place_pair(&mut children, sub_out, sup_out, cursor, w, false, ic);
                cursor += w;
            }
        } else {
            // RTL: [postscripts (reversed)] [base] [prescripts (reversed)]
            let mut cursor = if post_outputs.is_empty() {
                Au::zero()
            } else {
                params.space_after_script
            };
            for (i, ((sub_out, sup_out), &w)) in post_outputs
                .into_iter()
                .zip(&post_pair_widths)
                .enumerate()
                .rev()
            {
                let ic = if i == 0 { base_ic } else { Au::zero() };
                place_pair(&mut children, sub_out, sup_out, cursor, w, true, ic);
                cursor += w;
            }

            let base_x = cursor;
            translate_fragments(&mut base_out.fragments, base_x, total_ascent - base_ascent);
            children.extend(base_out.fragments);
            cursor += base_width;

            for ((sub_out, sup_out), &w) in pre_outputs.into_iter().zip(&pre_pair_widths).rev() {
                place_pair(&mut children, sub_out, sup_out, cursor, w, false, Au::zero());
                cursor += w;
            }
        }

        // Lay out any out-of-flow children.
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

        let result = MathLayoutResult::from_content(
            self.base(),
            containing_block,
            &pbm,
            children,
            MathBoxMetrics::new(content_width, content_block, total_ascent),
            None,
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
