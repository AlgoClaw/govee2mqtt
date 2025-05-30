	use crate::ble::{Base64HexBytes, SetSceneCode};
	use crate::lan_api::{Client, DiscoOptions, LanDevice as ActualLanDevice}; // Renamed to avoid conflict
	use crate::undoc_api::GoveeUndocumentedApi;
	use clap_num::maybe_hex;
	use std::collections::BTreeMap;
	use std::net::IpAddr;
	use uncased::Uncased;
	use anyhow::anyhow; // Added for anyhow::anyhow!

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
					let mut scene_code_by_name = BTreeMap::new();

					// Use the specific device's SKU
					let device_sku = device.sku.clone();

					let categories_from_api =
						GoveeUndocumentedApi::get_scenes_for_device(&device_sku).await?;

					for category_api_data in categories_from_api {
						for scene_api_data in category_api_data.scenes {
							let main_scene_name_str = &scene_api_data.scene_name;
							let mut added_combined_name_for_this_main_scene = false;

							let eligible_effects: Vec<_> = scene_api_data
								.light_effects
								.iter()
								.filter(|effect_entry| !effect_entry.scence_name.is_empty())
								.collect();

							if eligible_effects.len() >= 2 {
								for effect_entry in eligible_effects {
									let combined_name_str = format!("{}-{}", main_scene_name_str, effect_entry.scence_name);
									
									let param_string = effect_entry.scence_param.clone(); 

									scene_code_by_name.insert(
										Uncased::new(combined_name_str),
										SetSceneCode::new(effect_entry.scene_code, param_string, device_sku.clone()), // Added device_sku
									);
								}
								added_combined_name_for_this_main_scene = true;
							}

							if !added_combined_name_for_this_main_scene {
								let mut main_scene_added = false;
								if !scene_api_data.light_effects.is_empty() {
									for effect_entry in &scene_api_data.light_effects {
											let param_string = effect_entry.scence_param.clone();
											scene_code_by_name.insert(
												Uncased::new(main_scene_name_str.clone()),
												SetSceneCode::new(effect_entry.scene_code, param_string, device_sku.clone()), // Added device_sku
											);
											main_scene_added = true;
											break; 
									}
								}
								
								if !main_scene_added {
									 let code_u16 = match scene_api_data.scene_code.try_into() {
										 Ok(c) => c,
										 Err(_) => {
											 log::warn!("Scene code {} for '{}' out of u16 range, using 0", scene_api_data.scene_code, main_scene_name_str);
											 0 
										 }
									 };
									 if code_u16 != 0 { 
										 scene_code_by_name.insert(
											Uncased::new(main_scene_name_str.clone()),
											SetSceneCode::new(code_u16, String::new(), device_sku.clone()), // Added device_sku
										);
									 }
								}
							}
						}
					}

					if *list {
						for name_uncased in scene_code_by_name.keys() {
							println!("{}", name_uncased.as_str());
						}
					} else {
						let desired_scene_str = scene.clone().ok_or_else(|| anyhow!("Scene name must be provided if not listing"))?;
						let scene_key = Uncased::new(desired_scene_str); 

						if let Some(code_to_set) = scene_code_by_name.get(&scene_key) {
							// The SKU passed to encode_for_sku is for the PacketManager to find the correct codec.
							// SetSceneCode itself now holds its specific SKU internally for its encode method.
							let encoded =
								Base64HexBytes::encode_for_sku(&device_sku, code_to_set)?.base64();
							println!("Setting scene '{}'. Computed payload: {:?}", scene_key.as_str(), encoded);
							device.send_real(encoded).await?; 
						} else {
							anyhow::bail!("Scene '{}' not found. Available scenes: {:?}", 
								scene_key.as_str(), 
								scene_code_by_name.keys().map(|k| k.as_str()).collect::<Vec<&str>>()
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
