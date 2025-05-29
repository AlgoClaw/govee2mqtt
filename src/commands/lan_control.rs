use crate::ble::{Base64HexBytes, SetSceneCode};
use crate::lan_api::{Client, DiscoOptions, LanDevice as ActualLanDevice}; // Renamed to avoid conflict
use crate::undoc_api::GoveeUndocumentedApi;
use clap_num::maybe_hex;
use std::collections::BTreeMap;
use std::net::IpAddr;
use uncased::Uncased;
// Add other necessary imports like anyhow, log, etc., if they are used in this block.

#[derive(clap::Parser, Debug)]
pub struct LanControlCommand {
    #[arg(long)]
    pub ip: IpAddr,

    #[command(subcommand)]
    cmd: SubCommand,
}

#[derive(clap::Parser, Debug)]
enum SubCommand {
    On,
    Off,
    Brightness {
        percent: u8,
    },
    Temperature {
        kelvin: u32,
    },
    Color {
        color: csscolorparser::Color,
    },
    Command {
        #[arg(value_parser=maybe_hex::<u8>)]
        data: Vec<u8>,
    },
    Scene {
        #[arg(long)]
        list: bool,
        #[arg(required_unless_present = "list")]
        scene: Option<String>,
    },
}

impl LanControlCommand {
    pub async fn run(&self, _args: &crate::Args) -> anyhow::Result<()> {
        // Assuming _args might be used to get a StateHandle if GoveeUndocumentedApi client
        // needs to be retrieved from the state. For now, GoveeUndocumentedApi::get_scenes_for_device
        // is called as a static method. If it needs an instance, you'd get it here.
        // Example: let state = _args.get_state_handle_somehow();
        //          let undoc_client = state.get_undoc_client().await.ok_or_else(...)?.clone();

        let (client, _scan) = Client::new(DiscoOptions::default()).await?;
        let device: ActualLanDevice = client.scan_ip(self.ip).await?; // device is crate::lan_api::LanDevice

        match &self.cmd {
            SubCommand::On => {
                device.send_turn(true).await?;
            }
            SubCommand::Off => {
                device.send_turn(false).await?;
            }
            SubCommand::Brightness { percent } => {
                device.send_brightness(*percent).await?;
            }
            SubCommand::Temperature { kelvin } => {
                device.send_color_temperature_kelvin(*kelvin).await?;
            }
            SubCommand::Color { color } => {
                let [r, g, b, _a] = color.to_rgba8();
                device
                    .send_color_rgb(crate::lan_api::DeviceColor { r, g, b })
                    .await?;
            }
            SubCommand::Scene { list, scene } => { // Opening brace for the match arm
                let mut scene_code_by_name = BTreeMap::new();

                let categories_from_api =
                    GoveeUndocumentedApi::get_scenes_for_device(&device.sku).await?;

                for category_api_data in categories_from_api {
                    for scene_api_data in category_api_data.scenes {
                        let main_scene_name_str = &scene_api_data.scene_name;
                        let mut added_combined_name_for_this_main_scene = false;

                        let eligible_effects: Vec<_> = scene_api_data
                            .light_effects
                            .iter()
                            .filter(|effect_entry| !effect_entry.scence_name.is_empty() && effect_entry.scene_code != 0)
                            .collect();

                        if eligible_effects.len() >= 2 {
                            for effect_entry in eligible_effects {
                                let combined_name_str = format!("{}-{}", main_scene_name_str, effect_entry.scence_name);
                                
                                let param_string = effect_entry.scence_param.clone(); // Already a String

                                scene_code_by_name.insert(
                                    Uncased::new(combined_name_str),
                                    // effect_entry.scene_code is u16, scence_param is String
                                    SetSceneCode::new(effect_entry.scene_code, param_string),
                                );
                            }
                            added_combined_name_for_this_main_scene = true;
                        }

                        if !added_combined_name_for_this_main_scene {
                            let mut main_scene_added = false;
                            if !scene_api_data.light_effects.is_empty() {
                                for effect_entry in &scene_api_data.light_effects {
                                    if effect_entry.scene_code != 0 {
                                        let param_string = effect_entry.scence_param.clone();
                                        scene_code_by_name.insert(
                                            Uncased::new(main_scene_name_str.clone()),
                                            SetSceneCode::new(effect_entry.scene_code, param_string),
                                        );
                                        main_scene_added = true;
                                        break; 
                                    }
                                }
                            }
                            
                            if !main_scene_added && scene_api_data.scene_code != 0 {
                                 // scene_api_data.scene_code is u32, SetSceneCode::new expects u16 for code
                                 // and String for scence_param.
                                 let code_u16 = match scene_api_data.scene_code.try_into() {
                                     Ok(c) => c,
                                     Err(_) => {
                                         log::warn!("Scene code {} for '{}' out of u16 range, using 0", scene_api_data.scene_code, main_scene_name_str);
                                         0 // Default or skip
                                     }
                                 };
                                 if code_u16 != 0 { // Only add if the converted code is valid
                                     scene_code_by_name.insert(
                                        Uncased::new(main_scene_name_str.clone()),
                                        SetSceneCode::new(code_u16, String::new()), // Pass empty string for param
                                    );
                                 }
                            }
                        }
                    }
                }

                if *list {
                    for name_uncased in scene_code_by_name.keys() {
                        // Use .as_str() to get &str from &Uncased<String>
                        println!("{}", name_uncased.as_str());
                    }
                } else {
                    let desired_scene_str = scene.clone().ok_or_else(|| anyhow::anyhow!("Scene name must be provided if not listing"))?;
                    let scene_key = Uncased::new(desired_scene_str); // scene_key is Uncased<String>

                    if let Some(code_to_set) = scene_code_by_name.get(&scene_key) {
                        let encoded =
                            Base64HexBytes::encode_for_sku("Generic:Light", code_to_set)?.base64();
                        // Use .as_str() to get &str from &Uncased<String> (scene_key is Uncased<String>)
                        println!("Setting scene '{}'. Computed payload: {:?}", scene_key.as_str(), encoded);
                        device.send_real(encoded).await?; 
                    } else {
                        anyhow::bail!("Scene '{}' not found. Available scenes: {:?}", 
                            // Use .as_str() to get &str from &Uncased<String>
                            scene_key.as_str(), 
                            scene_code_by_name.keys().map(|k| k.as_str()).collect::<Vec<&str>>()
                        );
                    }
                }
            } // Closing brace for SubCommand::Scene
            SubCommand::Command { data } => {
                let encoded = Base64HexBytes::with_bytes(data.to_vec()).base64();
                println!("encoded: {encoded:?}");
                device.send_real(encoded).await?;
            }
        }

        Ok(())
    }
}
