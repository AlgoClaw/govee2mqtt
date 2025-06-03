use crate::undoc_api::{GoveeUndocumentedApi, LightEffectEntry}; // For API fallback
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File}; // Added fs for read_dir
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParsedScene {
    pub display_name: String,
    pub scene_code: u16, // For API scenes, or default for override
    pub api_scence_param: String, // For API scenes, empty for override
    pub sku: String,
    pub source_api_scene_name: String, // Name from API or override
    pub source_api_effect_name: Option<String>, // Only for API derived scenes with effects
    pub source_api_scene_id: u32,       // API scene ID, or default for override
    pub source_api_scence_param_id: u32, // API param ID, or default for override
    pub override_cmd_b64: Option<Vec<String>>, // Populated from JSON override
}

// Struct to represent an entry in the JSON override file (internal to this module)
#[derive(Debug, Clone, Deserialize)]
struct JsonSceneOverrideEntry {
    name: String,
    cmd_b64: Vec<String>, // This field in the JSON contains the final command lines
}

pub async fn get_parsed_scenes_for_sku(sku: &str) -> Result<Vec<ParsedScene>> {
    let override_dir = PathBuf::from("/JSONs");
    let mut found_override_file: Option<PathBuf> = None;

    if override_dir.is_dir() {
        match fs::read_dir(&override_dir) {
            Ok(entries) => {
                let mut matching_files = Vec::new();
                for entry in entries {
                    if let Ok(entry) = entry {
                        let path = entry.path();
                        if path.is_file() {
                            if let Some(filename_str) = path.file_name().and_then(|name| name.to_str()) {
                                if filename_str.contains(sku) && filename_str.to_lowercase().ends_with(".json") {
                                    matching_files.push(path.clone());
                                }
                            }
                        }
                    }
                }

                if !matching_files.is_empty() {
                    if matching_files.len() > 1 {
                        log::warn!(
                            "Multiple override files found for SKU '{}' in {:?}: {:?}. Using the first one: {:?}",
                            sku,
                            override_dir,
                            matching_files,
                            matching_files[0]
                        );
                    }
                    found_override_file = Some(matching_files[0].clone());
                }
            }
            Err(e) => {
                log::warn!("Failed to read override directory {:?}: {}", override_dir, e);
            }
        }
    } else {
        log::info!("Override directory {:?} does not exist or is not a directory.", override_dir);
    }


    if let Some(override_file_path) = found_override_file {
        log::info!("Attempting to load scenes from override file: {:?}", override_file_path);
        // Try to open and read the file
        let file = File::open(&override_file_path)
            .with_context(|| format!("Failed to open override file: {:?}", override_file_path))?;
        let reader = BufReader::new(file);

        // Parse the JSON content
        let json_scenes: Vec<JsonSceneOverrideEntry> = serde_json::from_reader(reader)
            .with_context(|| format!("Failed to parse JSON from override file: {:?}", override_file_path))?;

        // Convert JsonSceneOverrideEntry to ParsedScene
        let mut parsed_scenes: Vec<ParsedScene> = json_scenes
            .into_iter()
            .map(|json_entry| ParsedScene {
                display_name: json_entry.name.clone(),
                override_cmd_b64: Some(json_entry.cmd_b64), 
                api_scence_param: String::new(), 
                sku: sku.to_string(),
                scene_code: 0, 
                source_api_scene_name: json_entry.name, 
                source_api_effect_name: None,      
                source_api_scene_id: 0,            
                source_api_scence_param_id: 0,     
            })
            .collect();

        parsed_scenes.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        
        log::info!("Successfully loaded {} scenes from override file {:?} for SKU: {}", parsed_scenes.len(), override_file_path, sku);
        return Ok(parsed_scenes);
    } else {
         log::info!("No suitable override file found for SKU: {}. Falling back to API.", sku);
    }

    // Fallback to API if override file is not found
    let mut parsed_scenes_intermediate: Vec<ParsedScene> = Vec::new();
    // Ensure GoveeUndocumentedApi client is initialized if needed, or passed in.
    // For simplicity, assuming it can be instantiated here or is globally available.
    // If it requires specific initialization (e.g. with auth tokens), that needs to be handled.
    let categories_from_api = GoveeUndocumentedApi::get_scenes_for_device(sku).await?;

    for category_api_data in categories_from_api {
        for scene_api_data in &category_api_data.scenes {
            let main_api_scene_name = &scene_api_data.scene_name;
            let source_api_scene_id = scene_api_data.scene_id;
            let mut created_combined_name_for_this_main_scene = false;

            let eligible_effects_for_combined_name: Vec<&LightEffectEntry> = scene_api_data
                .light_effects
                .iter()
                .filter(|effect| !effect.scence_name.is_empty())
                .collect();

            if eligible_effects_for_combined_name.len() >= 2 {
                for effect_entry in eligible_effects_for_combined_name {
                    parsed_scenes_intermediate.push(ParsedScene {
                        display_name: format!("{}-{}", main_api_scene_name, effect_entry.scence_name),
                        scene_code: effect_entry.scene_code,
                        api_scence_param: effect_entry.scence_param.clone(),
                        sku: sku.to_string(),
                        source_api_scene_name: main_api_scene_name.clone(),
                        source_api_effect_name: Some(effect_entry.scence_name.clone()),
                        source_api_scene_id,
                        source_api_scence_param_id: effect_entry.scence_param_id,
                        override_cmd_b64: None, 
                    });
                }
                created_combined_name_for_this_main_scene = true;
            }

            if !created_combined_name_for_this_main_scene {
                if let Some(first_effect) = scene_api_data.light_effects.get(0) {
                    parsed_scenes_intermediate.push(ParsedScene {
                        display_name: main_api_scene_name.clone(),
                        scene_code: first_effect.scene_code,
                        api_scence_param: first_effect.scence_param.clone(),
                        sku: sku.to_string(),
                        source_api_scene_name: main_api_scene_name.clone(),
                        source_api_effect_name: if first_effect.scence_name.is_empty() {
                            None
                        } else {
                            Some(first_effect.scence_name.clone())
                        },
                        source_api_scene_id,
                        source_api_scence_param_id: first_effect.scence_param_id,
                        override_cmd_b64: None, 
                    });
                }
            }
        }
    }

    parsed_scenes_intermediate.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
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
    log::info!("Processed {} scenes from API for SKU: {}", final_scenes.len(), sku);
    Ok(final_scenes)
}
