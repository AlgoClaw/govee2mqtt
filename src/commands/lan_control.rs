use crate::ble::{Base64HexBytes, SetSceneCode};
use crate::lan_api::{Client, DiscoOptions, LanDevice as ActualLanDevice};
use crate::govee_scenes::get_parsed_scenes_for_sku; 
// GoveeUndocumentedApi is no longer directly used here for scenes
// use crate::undoc_api::GoveeUndocumentedApi; 
use clap_num::maybe_hex;
use std::net::IpAddr;
// use uncased::Uncased; // Removed unused import
use anyhow::anyhow;

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
				let parsed_scenes = get_parsed_scenes_for_sku(&device.sku).await?;

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
						let scene_to_set = SetSceneCode::new(
							target_scene.scene_code,
							target_scene.scence_param.clone(),
							target_scene.sku.clone(), // This is device.sku
						);
						
						let encoded_payload = Base64HexBytes::encode_for_sku(&device.sku, &scene_to_set)?.base64();
						println!("Setting scene '{}' for device {}. Computed payload: {:?}", target_scene.display_name, device.sku, encoded_payload);
						device.send_real(encoded_payload).await?;
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
				let encoded = Base64HexBytes::with_bytes(data.to_vec()).base64();
				println!("encoded: {encoded:?}");
				device.send_real(encoded).await?;
			}
		}
		Ok(())
	}
}