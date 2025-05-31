use crate::undoc_api::{GoveeUndocumentedApi, LightEffectEntry};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Represents a parsed scene, ready for display or use in commands.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParsedScene {
    /// The name to be displayed to the user or used for matching commands.
    /// Can be a main scene name or a combined name (e.g., "Scene-Effect").
    pub display_name: String,
    /// The numerical code for the scene or specific effect.
    pub scene_code: u16,
    /// The base64 encoded parameter string for the scene/effect.
    /// Can be empty if the scene is defined only by a code.
    pub scence_param: String,
    /// The SKU of the device this scene applies to.
    pub sku: String,
    /// The original main scene name from the Govee API (`LightEffectScene.scene_name`).
    pub source_api_scene_name: String,
    /// The original effect name from the Govee API (`LightEffectEntry.scence_name`), if applicable.
    pub source_api_effect_name: Option<String>,
    /// The original scene ID from the Govee API (`LightEffectScene.scene_id`).
    pub source_api_scene_id: u32,
    /// The original parameter ID from the Govee API (`LightEffectEntry.scence_param_id`).
    /// Will be 0 if the scene is not based on a specific LightEffectEntry (e.g., fallback to LightEffectScene.scene_code).
    pub source_api_scence_param_id: u32,
}

/// Fetches and parses the scenes for a given device SKU.
///
/// This function centralizes the logic for interpreting raw scene data from the
/// Govee Undocumented API, handling combined scene names and fallbacks.
/// The returned list is sorted by display_name and deduplicated.
pub async fn get_parsed_scenes_for_sku(sku: &str) -> Result<Vec<ParsedScene>> {
    let mut parsed_scenes: Vec<ParsedScene> = Vec::new();
    let categories_from_api = GoveeUndocumentedApi::get_scenes_for_device(sku).await?;

    for category_api_data in categories_from_api {
        for scene_api_data in category_api_data.scenes {
            let main_api_scene_name = &scene_api_data.scene_name;
            let source_api_scene_id = scene_api_data.scene_id;
            let mut created_combined_name_for_this_main_scene = false;

            // Filter for effects that have their own name and a valid scene_code.
            let eligible_effects_for_combined_name: Vec<&LightEffectEntry> = scene_api_data
                .light_effects
                .iter()
                .filter(|effect| !effect.scence_name.is_empty() && effect.scene_code != 0)
                .collect();

            // If there are enough distinct named effects, create combined names.
            // This threshold (e.g., >= 1 or >= 2) can be adjusted based on desired behavior.
            // Using >= 1 to list any individually named effect as a combined entry.
            // The logic from state_modified.rs used eligible_effects_for_combined_name.len() >= 2
            // Let's stick to that for consistency for now.
            if eligible_effects_for_combined_name.len() >= 2 {
                for effect_entry in &eligible_effects_for_combined_name {
                    parsed_scenes.push(ParsedScene {
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

            // If no combined names were generated for this main scene (based on the rule above),
            // add the main scene name itself, using its first valid effect.
            if !created_combined_name_for_this_main_scene {
                if let Some(first_valid_effect) = scene_api_data.light_effects.iter().find(|eff| eff.scene_code != 0) {
                    parsed_scenes.push(ParsedScene {
                        display_name: main_api_scene_name.clone(),
                        scene_code: first_valid_effect.scene_code,
                        scence_param: first_valid_effect.scence_param.clone(),
                        sku: sku.to_string(),
                        source_api_scene_name: main_api_scene_name.clone(),
                        source_api_effect_name: if first_valid_effect.scence_name.is_empty() { None } else { Some(first_valid_effect.scence_name.clone()) },
                        source_api_scene_id,
                        source_api_scence_param_id: first_valid_effect.scence_param_id,
                    });
                } else if scene_api_data.scene_code != 0 {
                    // Fallback: If the scene has no usable light_effects with scene_code != 0,
                    // but the scene itself has a non-zero scene_code (u32).
                    if let Ok(code_u16) = u16::try_from(scene_api_data.scene_code) {
                        if code_u16 != 0 { // Ensure the converted code is also non-zero
                            parsed_scenes.push(ParsedScene {
                                display_name: main_api_scene_name.clone(),
                                scene_code: code_u16,
                                scence_param: String::new(), // No specific effect param for this case
                                sku: sku.to_string(),
                                source_api_scene_name: main_api_scene_name.clone(),
                                source_api_effect_name: None,
                                source_api_scene_id,
                                source_api_scence_param_id: 0, // No specific effect param_id
                            });
                        }
                    } else {
                        // Log or handle the case where scene_api_data.scene_code (u32) is too large for u16
                        eprintln!(
                            "Warning: Scene '{}' (ID: {}) from SKU '{}' has a global scene_code {} which is out of u16 range. Skipping this fallback representation.",
                            main_api_scene_name, source_api_scene_id, sku, scene_api_data.scene_code
                        );
                    }
                }
            }
        }
    }

    // Sort by display_name for consistent ordering.
    parsed_scenes.sort_by(|a, b| a.display_name.cmp(&b.display_name));

    // Deduplicate entries based on the display_name.
    // If multiple effects or fallbacks somehow result in the same display_name,
    // this will keep the first one encountered after sorting.
    parsed_scenes.dedup_by(|a, b| a.display_name == b.display_name);

    Ok(parsed_scenes)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Mock GoveeUndocumentedApi for testing if needed, or use actual API calls for integration tests.
    // For now, this file assumes GoveeUndocumentedApi::get_scenes_for_device works as expected.

    #[test]
    fn parsed_scene_struct_fields() {
        let scene = ParsedScene {
            display_name: "Test Scene".to_string(),
            scene_code: 101,
            scence_param: "base64param".to_string(),
            sku: "HTEST".to_string(),
            source_api_scene_name: "API Scene Name".to_string(),
            source_api_effect_name: Some("Effect Name".to_string()),
            source_api_scene_id: 1,
            source_api_scence_param_id: 2,
        };
        assert_eq!(scene.display_name, "Test Scene");
        assert_eq!(scene.sku, "HTEST");
    }

    // Add more tests here to cover different scenarios from get_parsed_scenes_for_sku,
    // especially if you can mock the GoveeUndocumentedApi::get_scenes_for_device call.
    // Example:
    // async fn test_combined_name_generation() {
    //     // Mock GoveeUndocumentedApi to return specific data
    //     let scenes = get_parsed_scenes_for_sku("SOME_SKU_WITH_COMBINED_EFFECTS").await.unwrap();
    //     // Assertions on `scenes` content
    // }
}