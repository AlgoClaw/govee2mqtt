use crate::ble::{Base64HexBytes, SetSceneCode};
use crate::lan_api::{Client, DiscoOptions, LanDevice as ActualLanDevice};
use crate::govee_scenes::get_parsed_scenes_for_sku;
use anyhow::{anyhow, Context}; // Added Context
use clap_num::maybe_hex;
use std::net::IpAddr;

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
        let (client, _scan) = Client::new(DiscoOptions::default()).await?;
        let device: ActualLanDevice = client.scan_ip(self.ip).await?;

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
            SubCommand::Scene { list, scene } => {
                let parsed_scenes = get_parsed_scenes_for_sku(&device.sku).await
                    .with_context(|| format!("Failed to get parsed scenes for SKU {}", device.sku))?;

                if *list {
                    if parsed_scenes.is_empty() {
                        println!("No scenes found for device SKU: {}", device.sku);
                    } else {
                        println!("Available scenes for {}:", device.sku);
                        for scene_info in parsed_scenes {
                            println!("- {}", scene_info.display_name);
                        }
                    }
                } else {
                    let desired_scene_name_str = scene.as_ref().ok_or_else(|| anyhow!("Scene name must be provided if not listing"))?;

                    if let Some(target_scene) = parsed_scenes.iter().find(|s| s.display_name == *desired_scene_name_str) {
                        log::info!("Setting scene '{}' for device {} via LAN.", target_scene.display_name, device.sku);

                        if let Some(ref override_commands_b64) = target_scene.override_cmd_b64 {
                            log::info!("Using override LAN/BLE commands for scene: {}", target_scene.display_name);
                            // The send_real function on LanDevice expects Vec<String> of base64 commands.
                            // This matches the structure of override_cmd_b64.
                            device.send_real(override_commands_b64.clone()).await?;
                            println!("Successfully set scene '{}' using override commands.", target_scene.display_name);
                        } else if !target_scene.api_scence_param.is_empty() {
                            log::info!("Encoding API LAN/BLE commands for scene: {}", target_scene.display_name);
                            let scene_to_set = SetSceneCode::new(
                                target_scene.scene_code,
                                target_scene.api_scence_param.clone(), // Corrected field name
                                target_scene.sku.clone(), // This should be device.sku
                            );

                            // SetSceneCode::encode() returns a single Vec<u8> which might be multiple packets.
                            // Base64HexBytes::encode_for_sku also returns a Base64HexBytes struct that wraps this.
                            // The `base64()` method on Base64HexBytes then chunks it into Vec<String>.
                            match Base64HexBytes::encode_for_sku(&device.sku, &scene_to_set) {
                                Ok(encoded_command_container) => {
                                    let commands_b64 = encoded_command_container.base64();
                                    if !commands_b64.is_empty() {
                                        device.send_real(commands_b64).await?;
                                        println!("Successfully set scene '{}' using encoded API parameters.", target_scene.display_name);
                                    } else {
                                        anyhow::bail!("SetSceneCode::encode produced an empty command set for scene '{}'", target_scene.display_name);
                                    }
                                }
                                Err(e) => {
                                    anyhow::bail!("Failed to encode scene '{}' for LAN control: {}", target_scene.display_name, e);
                                }
                            }
                        } else {
                            anyhow::bail!("Scene '{}' found, but it has neither override commands nor API parameters for encoding.", target_scene.display_name);
                        }
                    } else {
                        let available_scene_names: Vec<&str> = parsed_scenes.iter().map(|s| s.display_name.as_str()).collect();
                        anyhow::bail!(
                            "Scene '{}' not found for device SKU '{}'. Available scenes: {:?}",
                            desired_scene_name_str,
                            device.sku,
                            available_scene_names
                        );
                    }
                }
            }
            SubCommand::Command { data } => {
                // This assumes data is raw bytes for a single command packet.
                // Base64HexBytes::with_bytes will pad and checksum it.
                // Its .base64() method will then produce a Vec<String> (likely with one element).
                let encoded = Base64HexBytes::with_bytes(data.to_vec()).base64();
                println!("Sending custom command. Encoded: {:?}", encoded);
                device.send_real(encoded).await?;
            }
        }
        Ok(())
    }
}
