use crate::undoc_api::{GoveeUndocumentedApi, LightEffectEntry};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParsedScene {
    pub display_name: String,
    pub scene_code: u16,
    pub scence_param: String,
    pub sku: String,
    pub source_api_scene_name: String,
    pub source_api_effect_name: Option<String>,
    pub source_api_scene_id: u32,
    pub source_api_scence_param_id: u32,
}

pub async fn get_parsed_scenes_for_sku(sku: &str) -> Result<Vec<ParsedScene>> {
    let mut parsed_scenes_intermediate: Vec<ParsedScene> = Vec::new();
    let categories_from_api = GoveeUndocumentedApi::get_scenes_for_device(sku).await?;

    for category_api_data in categories_from_api {
        for scene_api_data in &category_api_data.scenes { 
            let main_api_scene_name = &scene_api_data.scene_name;
            let source_api_scene_id = scene_api_data.scene_id;
            let mut created_combined_name_for_this_main_scene = false;

            // Filter for effects that have their own name. scene_code can now be 0.
            let eligible_effects_for_combined_name: Vec<&LightEffectEntry> = scene_api_data
                .light_effects
                .iter()
                .filter(|effect| !effect.scence_name.is_empty()) // Removed "&& effect.scene_code != 0"
                .collect();

            if eligible_effects_for_combined_name.len() >= 2 {
                for effect_entry in eligible_effects_for_combined_name {
                    parsed_scenes_intermediate.push(ParsedScene {
                        display_name: format!("{}-{}", main_api_scene_name, effect_entry.scence_name),
                        scene_code: effect_entry.scene_code,
                        scence_param: effect_entry.scence_param.clone(),
                        sku: sku.to_string(),
                        source_api_scene_name: main_api_scene_name.clone(),
                        source_api_effect_name: Some(effect_entry.scence_name.clone()),
                        source_api_scene_id,
                        source_api_scence_param_id: effect_entry.scence_param_id,
                    });
                }
                created_combined_name_for_this_main_scene = true;
            }

            if !created_combined_name_for_this_main_scene {
                // Use the first light effect if available, regardless of its scene_code value,
                // as scene_code 0 is now considered potentially valid for effects.
                if let Some(first_effect) = scene_api_data.light_effects.get(0) {
                    parsed_scenes_intermediate.push(ParsedScene {
                        display_name: main_api_scene_name.clone(),
                        scene_code: first_effect.scene_code,
                        scence_param: first_effect.scence_param.clone(),
                        sku: sku.to_string(),
                        source_api_scene_name: main_api_scene_name.clone(),
                        source_api_effect_name: if first_effect.scence_name.is_empty() { None } else { Some(first_effect.scence_name.clone()) },
                        source_api_scene_id,
                        source_api_scence_param_id: first_effect.scence_param_id,
                    });
                }
            }
        }
    }

    parsed_scenes_intermediate.sort_by(|a, b| {
        a.display_name.cmp(&b.display_name)
        .then_with(|| a.source_api_scene_id.cmp(&b.source_api_scene_id)) 
        .then_with(|| a.source_api_scence_param_id.cmp(&b.source_api_scence_param_id))
    });
    
    let mut final_scenes: Vec<ParsedScene> = Vec::new();
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    let mut base_name_occurrences: HashMap<String, usize> = HashMap::new();

    for scene in &parsed_scenes_intermediate {
        *base_name_occurrences.entry(scene.display_name.clone()).or_insert(0) += 1;
    }

    for mut scene in parsed_scenes_intermediate {
        let base_name = scene.display_name.clone();
        let total_occurrences = base_name_occurrences.get(&base_name).cloned().unwrap_or(0);
        
        if total_occurrences > 1 {
            let count = name_counts.entry(base_name.clone()).or_insert(0);
            *count += 1;
            scene.display_name = format!("{} ({})", base_name, *count);
        }
        final_scenes.push(scene);
    }
    
    final_scenes.sort_by(|a, b| a.display_name.cmp(&b.display_name));

    Ok(final_scenes)
}