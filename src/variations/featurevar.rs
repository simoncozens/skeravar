use std::hash::Hash;

use crate::FastHashMap;
use font_types::{F2Dot14, MajorMinor, Offset32};
use read_fonts::{
    tables::{
        gsub::Gsub,
        layout::{
            Condition, ConditionSet, FeatureTableSubstitution, FeatureTableSubstitutionRecord,
            FeatureVariationRecord, FeatureVariations,
        },
    },
    MinByteRange,
};
use skrifa::{
    raw::{
        collections::IntSet,
        tables::{gsub::FeatureList, layout::ConditionFormat1},
        FontData, ReadError, TopLevelTable,
    },
    Tag,
};

use crate::{
    layout::SubsetLayoutContext,
    offset::SerializeSubset,
    offset_array::SubsetOffsetArray,
    serialize::{SerializeErrorFlags, SerializeResultEmpty, Serializer},
    variations::solver::{renormalize_value, Triple},
    Plan, SubsetError, SubsetTable,
};
enum CondWithVar {
    KeepCondition,
    KeepRecord,
    DropCondition,
    DropRecord,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct ConditionMap(FastHashMap<usize, u32>);
impl Hash for ConditionMap {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Custom hash implementation to ensure the order of conditions does not affect the hash
        let mut sorted_conditions: Vec<(&usize, &u32)> = self.0.iter().collect();
        sorted_conditions.sort_by_key(|&(k, _)| k);
        for (k, v) in sorted_conditions {
            k.hash(state);
            v.hash(state);
        }
    }
}

trait KeepWithVariations {
    fn keep_with_variations(
        &self,
        ctx: &mut CollectFeatureSubstitutesContext,
        map: &mut ConditionMap,
    ) -> CondWithVar;
}

pub(crate) struct CollectFeatureSubstitutesContext<'a> {
    pub(crate) axes_index_tag_map: &'a FastHashMap<usize, Tag>,
    pub(crate) axes_location: &'a FastHashMap<Tag, Triple>,
    pub(crate) record_cond_idx_map: FastHashMap<u16, IntSet<u16>>,
    pub(crate) catch_all_record_feature_indices: IntSet<u16>,

    pub(crate) feature_indices: IntSet<u16>,
    pub(crate) apply: bool,
    pub(crate) variation_applied: bool,
    pub(crate) universal: bool,
    pub(crate) cur_record_idx: u16,
    pub(crate) conditionset_map: FastHashMap<ConditionMap, u16>,
    /// For each feature index that has a substitute, store the lookup indices from that substitute
    pub(crate) feature_substitutes_lookup_map: FastHashMap<u16, IntSet<u16>>,
}

fn keep_with_variations(
    c: ConditionSet<'_>,
    ctx: &mut CollectFeatureSubstitutesContext,
) -> Result<(), ReadError> {
    // Expect a new empty map
    let mut map = ConditionMap::default();
    let mut cond_set = IntSet::default();

    ctx.apply = true;
    let mut should_keep = false;
    let mut num_kept_cond = 0;
    for (ix, condition) in c.conditions().iter().enumerate() {
        match condition?.keep_with_variations(ctx, &mut map) {
            CondWithVar::DropRecord => {
                return Ok(());
            }
            CondWithVar::KeepCondition => {
                should_keep = true;
                cond_set.insert(ix as u16);
                num_kept_cond += 1;
            }
            CondWithVar::KeepRecord => {
                should_keep = true;
            }
            CondWithVar::DropCondition => {}
        }
    }

    if !should_keep {
        return Ok(());
    }
    //check if condition_set is unique with variations
    if ctx.conditionset_map.contains_key(&map) {
        //duplicate found, drop the entire record
        return Ok(());
    }
    ctx.conditionset_map.insert(map, 1);
    ctx.record_cond_idx_map.insert(ctx.cur_record_idx, cond_set);
    if should_keep && num_kept_cond == 0 {
        ctx.universal = true;
    }
    Ok(())
}

impl KeepWithVariations for Condition<'_> {
    fn keep_with_variations(
        &self,
        ctx: &mut CollectFeatureSubstitutesContext,
        map: &mut ConditionMap,
    ) -> CondWithVar {
        match self {
            Condition::Format1AxisRange(c) => c.keep_with_variations(ctx, map),
            _ => {
                ctx.apply = false;
                CondWithVar::KeepCondition
            }
        }
    }
}

impl KeepWithVariations for ConditionFormat1<'_> {
    fn keep_with_variations(
        &self,
        ctx: &mut CollectFeatureSubstitutesContext,
        map: &mut ConditionMap,
    ) -> CondWithVar {
        //invalid axis index, drop the entire record
        let axis_index = self.axis_index() as usize;
        let Some(axis_tag) = ctx.axes_index_tag_map.get(&axis_index) else {
            return CondWithVar::DropRecord;
        };

        let (axis_range, set_by_user): (Triple, bool) =
            if let Some(axis_limit) = ctx.axes_location.get(axis_tag) {
                (*axis_limit, true)
            } else {
                (Triple::default(), false)
            };

        let axis_min_val = axis_range.minimum;
        let axis_default_val = axis_range.middle;
        let axis_max_val = axis_range.maximum;

        let filter_min_val = self.filter_range_min_value().to_f32() as f64;
        let filter_max_val = self.filter_range_max_value().to_f32() as f64;

        // log::debug!("Filter min/max: {filter_min_val}, {filter_max_val}");
        // log::debug!("Axis min/default/max: {axis_min_val}, {axis_default_val}, {axis_max_val}");

        if axis_default_val < filter_min_val || axis_default_val > filter_max_val {
            // log::debug!("  Don't apply");
            ctx.apply = false;
        }

        //condition not met, drop the entire record
        if axis_min_val > filter_max_val
            || axis_max_val < filter_min_val
            || filter_min_val > filter_max_val
        {
            // log::debug!("  Condition not met, drop the record");
            return CondWithVar::DropRecord;
        }

        //condition met and axis pinned, drop the condition
        if set_by_user && axis_range.is_point() {
            // log::debug!("  Condition met and axis pinned, drop the condition");
            return CondWithVar::DropCondition;
        }

        if filter_max_val != axis_max_val || filter_min_val != axis_min_val {
            // add axisIndex->value into the hashmap so we can check if the record is
            // unique with variations
            let int_filter_max_val = self.filter_range_max_value().to_bits() as u16; // Unsure about this cast
            let int_filter_min_val = self.filter_range_min_value().to_bits() as u16;
            let val: u32 = ((int_filter_max_val as u32) << 16) + (int_filter_min_val as u32);

            map.0.insert(axis_index, val);
            // log::debug!("  Condition met, keep the condition");
            return CondWithVar::KeepCondition;
        }

        CondWithVar::KeepRecord
    }
}

pub(crate) fn collect_feature_substitutes_with_variations(
    feature_variations: &FeatureVariations,
    ctx: &mut CollectFeatureSubstitutesContext,
) -> Result<(), SubsetError> {
    // Not returning anything, just modify context
    let data = feature_variations.offset_data();

    for (record_idx, record) in feature_variations
        .feature_variation_records()
        .iter()
        .enumerate()
    {
        ctx.cur_record_idx = record_idx as u16;

        // varRecords[i].collect_feature_substitutes_with_variations (c, this)
        if let Some(Ok(cond_set)) = record.condition_set(data) {
            keep_with_variations(cond_set, ctx).map_err(SubsetError::ReadError)?;
            // log::debug!(
            //     "Record {}: kept conditions {:?} after applying variations",
            //     record_idx,
            //     &ctx.record_cond_idx_map.get(&ctx.cur_record_idx)
            // );

            if ctx.apply && !ctx.variation_applied {
                // Extract feature substitutes from FeatureTableSubstitution
                // (base+substitutions).collect_feature_substitutes_with_variations (c);
                if let Some(Ok(subs)) = record.feature_table_substitution(data) {
                    for sub_rec in subs.substitutions() {
                        // record.collect_feature_substitutes_with_variations
                        let feature_index = sub_rec.feature_index();
                        if ctx.feature_indices.contains(feature_index) {
                            // Extract lookup indices from the substitute feature
                            if let Ok(alternate_feature) =
                                sub_rec.alternate_feature(subs.offset_data())
                            {
                                let mut lookups = IntSet::default();
                                for lookup_idx in alternate_feature.lookup_list_indices() {
                                    lookups.insert(lookup_idx.get());
                                }
                                ctx.feature_substitutes_lookup_map
                                    .insert(feature_index, lookups);
                                ctx.catch_all_record_feature_indices.insert(feature_index);
                            }
                        }
                    }
                }
                ctx.variation_applied = true; // Set variations only once
            }
        }
        if ctx.universal {
            break;
        }
    }

    if ctx.universal || ctx.record_cond_idx_map.is_empty() {
        ctx.catch_all_record_feature_indices.clear();
    }

    Ok(())
}

pub(crate) struct FeatureSubstituteCollectionResult {
    pub(crate) catch_all_record_feature_indices: IntSet<u16>,
    /// For each feature index that has a substitute, store the lookup indices from that substitute
    pub(crate) feature_substitutes_lookup_map: FastHashMap<u16, IntSet<u16>>,
    // pub(crate) variation_applied: bool,
    pub(crate) record_cond_idx_map: FastHashMap<u16, IntSet<u16>>,
}
impl FeatureSubstituteCollectionResult {
    pub(crate) fn empty() -> Self {
        Self {
            catch_all_record_feature_indices: IntSet::default(),
            feature_substitutes_lookup_map: FastHashMap::default(),
            // variation_applied: false,
            record_cond_idx_map: FastHashMap::default(),
        }
    }
}

impl From<CollectFeatureSubstitutesContext<'_>> for FeatureSubstituteCollectionResult {
    fn from(val: CollectFeatureSubstitutesContext<'_>) -> Self {
        FeatureSubstituteCollectionResult {
            catch_all_record_feature_indices: val.catch_all_record_feature_indices,
            feature_substitutes_lookup_map: val.feature_substitutes_lookup_map,
            // variation_applied: val.variation_applied,
            record_cond_idx_map: val.record_cond_idx_map,
        }
    }
}

/// Collect lookups from features, using substitutes where they exist
pub(crate) fn collect_lookups_with_substitutes(
    // gsub: &Gsub,
    feature_list: FeatureList<'_>,
    feature_indices: &IntSet<u16>,
    feature_substitutes: &FeatureSubstituteCollectionResult,
) -> Result<IntSet<u16>, SubsetError> {
    let mut lookup_indices = IntSet::empty();

    for feature_index in feature_indices.iter() {
        // Check if this feature has a substitute from feature variations
        if let Some(substitute_lookups) = feature_substitutes
            .feature_substitutes_lookup_map
            .get(&feature_index)
        {
            // Use the substitute feature's lookups
            lookup_indices.extend(substitute_lookups.iter());
        } else {
            // Use the default feature's lookups
            if let Some(record) = feature_list.feature_records().get(feature_index as usize) {
                if let Ok(feature) = record.feature(feature_list.offset_data()) {
                    for lookup_idx in feature.lookup_list_indices() {
                        lookup_indices.insert(lookup_idx.get());
                    }
                }
            }
        }
    }

    Ok(lookup_indices)
}

/// Collect lookups from feature variations whose conditions matched
pub(crate) fn feature_variation_collect_lookups(
    feature_variations: &FeatureVariations,
    feature_indices: &IntSet<u16>,
    feature_record_cond_idx_map: &FastHashMap<u16, IntSet<u16>>,
    lookup_indices: &mut IntSet<u16>,
) -> Result<(), SubsetError> {
    let var_records = feature_variations.feature_variation_records();
    // Offsets in each record are relative to the FeatureVariations table, not to
    // the enclosing GSUB/GPOS table (matching HarfBuzz, which uses
    // `varRecords[i].collect_lookups(this, ...)` with `this` = FeatureVariations).
    let data = feature_variations.offset_data();

    for (record_idx, var_record) in var_records.iter().enumerate() {
        // Only process records whose conditions matched (are in the map)
        if !feature_record_cond_idx_map.contains_key(&(record_idx as u16)) {
            continue;
        }

        if let Some(feature_subs) = var_record
            .feature_table_substitution(data)
            .transpose()
            .ok()
            .flatten()
        {
            for sub_rec in feature_subs.substitutions() {
                let feature_index = sub_rec.feature_index();

                // Only collect if this is a feature we're keeping
                if !feature_indices.contains(feature_index) {
                    continue;
                }

                if let Ok(substitute_feature) =
                    sub_rec.alternate_feature(feature_subs.offset_data())
                {
                    for lookup_idx in substitute_feature.lookup_list_indices() {
                        lookup_indices.insert(lookup_idx.get());
                    }
                }
            }
        }
    }

    Ok(())
}
