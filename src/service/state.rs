use crate::ble::{Base64HexBytes, SetHumidifierMode, SetHumidifierNightlightParams, SetSceneCode};
use crate::lan_api::{Client as LanClient, DeviceStatus as LanDeviceStatus, LanDevice};
use crate::platform_api::{DeviceCapability, GoveeApiClient};
use crate::service::coordinator::Coordinator;
use crate::service::device::Device;
use crate::service::hass::{topic_safe_id, HassClient};
use crate::service::iot::IotClient;
use crate::temperature::{TemperatureScale, TemperatureValue};
use crate::govee_scenes::{get_parsed_scenes_for_sku, ParsedScene}; // Import ParsedScene and the function
use anyhow::Context;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard, Semaphore};
use tokio::time::{sleep, Duration};

// Definitions for ParsedScene and JsonSceneOverrideEntry are now solely in govee_scenes.rs

#[derive(Default)]
pub struct State {
    devices_by_id: Mutex<HashMap<String, Device>>,
    semaphore_by_id: Mutex<HashMap<String, Arc<Semaphore>>>,
    lan_client: Mutex<Option<LanClient>>,
    platform_client: Mutex<Option<GoveeApiClient>>,
    #[allow(dead_code)]
    undoc_client: Mutex<Option<crate::undoc_api::GoveeUndocumentedApi>>,
    iot_client: Mutex<Option<IotClient>>,
    hass_client: Mutex<Option<HassClient>>,
    hass_discovery_prefix: Mutex<String>,
    temperature_scale: Mutex<TemperatureScale>,
}

pub type StateHandle = Arc<State>;

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set_temperature_scale(&self, scale: TemperatureScale) {
        *self.temperature_scale.lock().await = scale;
    }

    pub async fn get_temperature_scale(&self) -> TemperatureScale {
        *self.temperature_scale.lock().await
    }

    pub async fn set_hass_disco_prefix(&self, prefix: String) {
        *self.hass_discovery_prefix.lock().await = prefix;
    }

    pub async fn get_hass_disco_prefix(&self) -> String {
        self.hass_discovery_prefix.lock().await.to_string()
    }

    pub async fn device_mut(&self, sku: &str, id: &str) -> MappedMutexGuard<Device> {
        let devices = self.devices_by_id.lock().await;
        MutexGuard::map(devices, |devices| {
            devices
                .entry(id.to_string())
                .or_insert_with(|| Device::new(sku, id))
        })
    }

    pub async fn devices(&self) -> Vec<Device> {
        self.devices_by_id.lock().await.values().cloned().collect()
    }

    pub async fn device_by_id(&self, id: &str) -> Option<Device> {
        let devices = self.devices_by_id.lock().await;
        devices.get(id).cloned()
    }

    async fn semaphore_for_device(&self, device: &Device) -> Arc<Semaphore> {
        self.semaphore_by_id
            .lock()
            .await
            .entry(device.id.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }

    pub async fn resolve_device_read_only(self: &Arc<Self>, label: &str) -> anyhow::Result<Device> {
        self.resolve_device(label)
            .await
            .ok_or_else(|| anyhow::anyhow!("device '{label}' not found"))
    }

    pub async fn resolve_device_for_control(
        self: &Arc<Self>,
        label: &str,
    ) -> anyhow::Result<Coordinator> {
        let device = self
            .resolve_device(label)
            .await
            .ok_or_else(|| anyhow::anyhow!("device '{label}' not found"))?;
        let semaphore = self.semaphore_for_device(&device).await;
        let permit = semaphore.acquire_owned().await?;
        let (tx, rx) = tokio::sync::oneshot::channel();

        let state = self.clone();
        let device_id = device.id.to_string();
        tokio::spawn(async move {
            let _ = rx.await;
            state.poll_after_control(device_id).await
        });

        Ok(Coordinator::new(device, permit, tx))
    }

    pub async fn resolve_device(&self, label: &str) -> Option<Device> {
        let devices = self.devices_by_id.lock().await;

        if let Some(device) = devices.get(label) {
            return Some(device.clone());
        }

        for d in devices.values() {
            if d.name().eq_ignore_ascii_case(label)
                || d.id.eq_ignore_ascii_case(label)
                || topic_safe_id(d).eq_ignore_ascii_case(label)
                || d.ip_addr()
                    .map(|ip| ip.to_string().eq_ignore_ascii_case(label))
                    .unwrap_or(false)
                || d.computed_name().eq_ignore_ascii_case(label)
            {
                return Some(d.clone());
            }
        }

        None
    }

    pub async fn set_hass_client(&self, client: HassClient) {
        self.hass_client.lock().await.replace(client);
    }

    pub async fn get_hass_client(&self) -> Option<HassClient> {
        self.hass_client.lock().await.clone()
    }

    pub async fn set_iot_client(&self, client: IotClient) {
        self.iot_client.lock().await.replace(client);
    }

    pub async fn get_iot_client(&self) -> Option<IotClient> {
        self.iot_client.lock().await.clone()
    }

    pub async fn set_lan_client(&self, client: LanClient) {
        self.lan_client.lock().await.replace(client);
    }

    pub async fn get_lan_client(&self) -> Option<LanClient> {
        self.lan_client.lock().await.clone()
    }

    pub async fn set_platform_client(&self, client: GoveeApiClient) {
        self.platform_client.lock().await.replace(client);
    }

    pub async fn get_platform_client(&self) -> Option<GoveeApiClient> {
        self.platform_client.lock().await.clone()
    }

    pub async fn set_undoc_client(&self, client: crate::undoc_api::GoveeUndocumentedApi) {
        self.undoc_client.lock().await.replace(client);
    }

    #[allow(dead_code)]
    pub async fn get_undoc_client(&self) -> Option<crate::undoc_api::GoveeUndocumentedApi> {
        self.undoc_client.lock().await.clone()
    }

    pub async fn poll_iot_api(self: &Arc<Self>, device: &Device) -> anyhow::Result<bool> {
        if let Some(iot) = self.get_iot_client().await {
            if let Some(info) = device.undoc_device_info.clone() {
                if iot.is_device_compatible(&info.entry) {
                    let device_state = device.device_state();
                    log::info!("requesting update via IoT MQTT {device} {device_state:?}");
                    match iot
                        .request_status_update(&info.entry)
                        .await
                        .context("iot.request_status_update")
                    {
                        Err(err) => {
                            log::error!("Failed: {err:#}");
                        }
                        Ok(()) => {
                            self.device_mut(&device.sku, &device.id)
                                .await
                                .set_last_polled();
                            return Ok(true);
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    pub async fn poll_platform_api(self: &Arc<Self>, device: &Device) -> anyhow::Result<bool> {
        if let Some(client) = self.get_platform_client().await {
            let device_state = device.device_state();
            log::info!("requesting update via Platform API {device} {device_state:?}");
            if let Some(info) = &device.http_device_info {
                let http_state = client
                    .get_device_state(info)
                    .await
                    .context("get_device_state")?;
                log::trace!("updated state for {device}");

                {
                    let mut device_mut = self.device_mut(&device.sku, &device.id).await;
                    device_mut.set_http_device_state(http_state);
                    device_mut.set_last_polled();
                }
                self.notify_of_state_change(&device.id)
                    .await
                    .context("state.notify_of_state_change")?;
                return Ok(true);
            }
        } else {
            log::trace!(
                "device {device} wanted a status update, but there is no platform client available"
            );
        }
        Ok(false)
    }

    async fn poll_lan_api<F: Fn(&LanDeviceStatus) -> bool>(
        self: &Arc<Self>,
        device: &LanDevice,
        acceptor: F,
    ) -> anyhow::Result<()> {
        match self.get_lan_client().await {
            Some(client) => {
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() <= deadline {
                    let status = client.query_status(device).await?;
                    let accepted = (acceptor)(&status);
                    self.device_mut(&device.sku, &device.device)
                        .await
                        .set_lan_device_status(status);
                    if accepted {
                        break;
                    }
                    sleep(Duration::from_millis(100)).await;
                }
                self.notify_of_state_change(&device.device).await?;
                Ok(())
            }
            None => anyhow::bail!("no lan client"),
        }
    }

    pub async fn device_control<V: Into<JsonValue>>(
        self: &Arc<Self>,
        device: &Device,
        capability: &DeviceCapability,
        value: V,
    ) -> anyhow::Result<()> {
        let value: JsonValue = value.into();
        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to send {value:?} control to {device}");
                client.control_device(info, capability, value).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to use Platform API to control {device}");
    }

    pub async fn device_light_power_on(
        self: &Arc<Self>,
        device: &Device,
        on: bool,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| p.on = on)
            .await?
        {
            return Ok(());
        }

        let instance_name = device
            .get_light_power_toggle_instance_name()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Don't know how to toggle just the light portion of {device}. \
                     Please share the device metadata and state if you report this issue"
                )
            })?;

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} light power state");
            lan_dev.send_turn(on).await?;
            self.poll_lan_api(lan_dev, |status| status.on == on).await?;
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} light power state");
                    iot.set_power_state(&info.entry, on).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} light {instance_name} state");
                client.set_toggle_state(info, instance_name, on).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to control light power state for {device}");
    }

    pub async fn device_power_on(
        self: &Arc<Self>,
        device: &Device,
        on: bool,
    ) -> anyhow::Result<()> {
        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} power state");
            lan_dev.send_turn(on).await?;
            self.poll_lan_api(lan_dev, |status| status.on == on).await?;
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} power state");
                    iot.set_power_state(&info.entry, on).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} power state");
                client.set_power_state(info, on).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to control power state for {device}");
    }

    pub async fn device_set_brightness(
        self: &Arc<Self>,
        device: &Device,
        percent: u8,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| {
                p.brightness = percent;
                p.on = true;
            })
            .await?
        {
            return Ok(());
        }

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} brightness");
            lan_dev.send_brightness(percent).await?;
            self.poll_lan_api(lan_dev, |status| status.brightness == percent)
                .await?;
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} brightness");
                    iot.set_brightness(&info.entry, percent).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} brightness");
                client.set_brightness(info, percent).await?;
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control brightness for {device}");
    }

    pub async fn device_set_color_temperature(
        self: &Arc<Self>,
        device: &Device,
        kelvin: u32,
    ) -> anyhow::Result<()> {
        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} color temperature");
            lan_dev.send_color_temperature_kelvin(kelvin).await?;
            self.poll_lan_api(lan_dev, |status| status.color_temperature_kelvin == kelvin)
                .await?;
            self.device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(None);
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} color temperature");
                    iot.set_color_temperature(&info.entry, kelvin).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} color temperature");
                client.set_color_temperature(info, kelvin).await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene(None);
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control color temperature for {device}");
    }

    async fn try_humidifier_set_nightlight<F: Fn(&mut SetHumidifierNightlightParams)>(
        self: &Arc<Self>,
        device: &Device,
        apply: F,
    ) -> anyhow::Result<bool> {
        let mut params: SetHumidifierNightlightParams =
            device.nightlight_state.clone().unwrap_or_default().into();
        (apply)(&mut params);

        if let Ok(command) = Base64HexBytes::encode_for_sku(&device.sku, &params) {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} color (via humidifier nightlight)");
                    iot.send_real(&info.entry, command.base64()).await?;
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    pub async fn humidifier_set_parameter(
        self: &Arc<Self>,
        device: &Device,
        work_mode: i64,
        value: i64,
    ) -> anyhow::Result<()> {
        if let Ok(command) = Base64HexBytes::encode_for_sku(
            &device.sku,
            &SetHumidifierMode {
                mode: work_mode as u8,
                param: value as u8,
            },
        ) {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    iot.send_real(&info.entry, command.base64()).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                client.set_work_mode(info, work_mode, value).await?;
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control humidifier parameter work_mode={work_mode} for {device}");
    }

    pub async fn device_set_color_rgb(
        self: &Arc<Self>,
        device: &Device,
        r: u8,
        g: u8,
        b: u8,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| {
                p.r = r;
                p.g = g;
                p.b = b;
                p.on = true;
            })
            .await?
        {
            return Ok(());
        }

        if let Some(lan_dev) = &device.lan_device {
            let color = crate::lan_api::DeviceColor { r, g, b };
            log::info!("Using LAN API to set {device} color");
            lan_dev.send_color_rgb(color).await?;
            self.poll_lan_api(lan_dev, |status| status.color == color)
                .await?;
            self.device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(None);
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} color");
                    iot.set_color_rgb(&info.entry, r, g, b).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} color");
                client.set_color_rgb(info, r, g, b).await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene(None);
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control color for {device}");
    }

    pub async fn poll_after_control(self: &Arc<Self>, id: String) {
        let Some(device) = self.device_by_id(&id).await else {
            return;
        };

        let iot_available = self.get_iot_client().await.is_some();

        if device.pollable_via_iot() && iot_available {
            return;
        }
        if device.pollable_via_lan() {
            return;
        }

        sleep(Duration::from_secs(5)).await;

        log::info!("Polling {device} to get latest state after control");
        if let Err(err) = self.poll_platform_api(&device).await {
            log::error!("Polling {device} failed: {err:#}");
        }
    }

    pub async fn device_list_scenes(&self, device: &Device) -> anyhow::Result<Vec<String>> {
        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                let platform_scenes = client.list_scene_names(info).await?;
                if !platform_scenes.is_empty() {
                    return Ok(sort_and_dedup_scenes(platform_scenes));
                }
            }
        }
        match get_parsed_scenes_for_sku(&device.sku).await { // Use imported function directly
            Ok(parsed_scenes) => {
                let names: Vec<String> = parsed_scenes.into_iter().map(|s| s.display_name).collect();
                if !names.is_empty() {
                    return Ok(sort_and_dedup_scenes(names));
                }
            }
            Err(e) => {
                log::warn!(
                    "Failed to get scenes via centralized parser for {}: {}. Platform API was also unavailable or didn't provide scenes.",
                    device, e
                );
            }
        }
        log::trace!("Platform API and centralized scene parser returned no scenes for {device}");
        Ok(vec![])
    }


    pub async fn device_set_target_temperature(
        self: &Arc<Self>,
        device: &Device,
        instance_name: &str,
        target: TemperatureValue,
    ) -> anyhow::Result<()> {
        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} target temperature to {target}");
                client
                    .set_target_temperature(info, instance_name, target)
                    .await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to set temperature for {device}");
    }

    pub async fn device_set_scene(
        self: &Arc<Self>,
        device: &Device,
        scene_name_to_set: &str,
    ) -> anyhow::Result<()> {
        let avoid_platform_api = device.avoid_platform_api();

        if !avoid_platform_api {
            if let Some(client) = self.get_platform_client().await {
                if let Some(info) = &device.http_device_info {
                    log::info!("Using Platform API to set {device} to scene {scene_name_to_set}");
                    match client.set_scene_by_name(info, scene_name_to_set).await {
                        Ok(_) => {
                            self.device_mut(&device.sku, &device.id)
                                .await
                                .set_active_scene(Some(scene_name_to_set));
                            return Ok(());
                        }
                        Err(e) => {
                            log::warn!("Platform API failed to set scene {scene_name_to_set} for {device}: {e}. Trying other methods.");
                        }
                    }
                }
            }
        }

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} to scene {scene_name_to_set}");
            match lan_dev.set_scene_by_name(scene_name_to_set).await {
                Ok(_) => {
                    self.device_mut(&device.sku, &device.id)
                        .await
                        .set_active_scene(Some(scene_name_to_set));
                    return Ok(());
                }
                Err(e) => {
                    log::warn!("LAN API failed to set scene {scene_name_to_set} for {device}: {e}. Trying other methods.");
                }
            }
        }

        log::info!("Attempting to set scene '{scene_name_to_set}' for {device} via BLE/IoT.");
        let all_parsed_scenes = get_parsed_scenes_for_sku(&device.sku).await // Use imported function directly
            .with_context(|| format!("Failed to get parsed scenes for SKU {} to set scene via BLE", device.sku))?;

        if let Some(target_scene) = all_parsed_scenes.into_iter().find(|ps: &ParsedScene| ps.display_name == scene_name_to_set) { // ParsedScene type from import
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    if let Some(ref override_commands_b64) = target_scene.override_cmd_b64 {
                        log::info!("Using override BLE commands for scene: {}", target_scene.display_name);
                        iot.send_real(&info.entry, override_commands_b64.clone()).await?;
                        self.device_mut(&device.sku, &device.id)
                            .await
                            .set_active_scene(Some(scene_name_to_set));
                        return Ok(());
                    } else if !target_scene.api_scence_param.is_empty() {
                        log::info!("Encoding API BLE commands for scene: {}", target_scene.display_name);
                        let scene_encoder = SetSceneCode::new(
                            target_scene.scene_code,
                            target_scene.api_scence_param.clone(),
                            device.sku.to_string(),
                        );
                        match scene_encoder.encode() {
                            Ok(encoded_byte_stream) => {
                                let commands_b64: Vec<String> = encoded_byte_stream.chunks(20)
                                    .map(|chunk| data_encoding::BASE64.encode(chunk))
                                    .collect();

                                if !commands_b64.is_empty() {
                                    iot.send_real(&info.entry, commands_b64).await?;
                                    self.device_mut(&device.sku, &device.id)
                                        .await
                                        .set_active_scene(Some(scene_name_to_set));
                                    return Ok(());
                                } else {
                                    log::error!("SetSceneCode::encode produced empty command for {}: {}", device, scene_name_to_set);
                                }
                            }
                            Err(e) => {
                                log::error!("Failed to encode scene {} for {device} using SetSceneCode: {e}", scene_name_to_set);
                            }
                        }
                    } else {
                        log::warn!("Scene '{scene_name_to_set}' found for {device}, but it has neither override commands nor API parameters for BLE encoding.");
                    }
                } else {
                    log::warn!("IoT client or Govee device info not available for BLE scene control for {device}.");
                }
            } else {
                 log::warn!("IoT client not available for BLE scene control for {device}.");
            }
        } else {
            log::warn!("Scene '{scene_name_to_set}' not found in parsed scenes for SKU {} of device {device}.", device.sku);
        }

        anyhow::bail!("Unable to set scene '{scene_name_to_set}' for {device} using any available method.");
    }


    pub async fn notify_of_state_change(self: &Arc<Self>, device_id: &str) -> anyhow::Result<()> {
        let Some(canonical_device) = self.device_by_id(&device_id).await else {
            anyhow::bail!("cannot find device {device_id}!?");
        };

        if let Some(hass) = self.get_hass_client().await {
            hass.advise_hass_of_light_state(&canonical_device, self)
                .await?;
        }

        Ok(())
    }
}

pub fn sort_and_dedup_scenes(mut scenes: Vec<String>) -> Vec<String> {
    scenes.sort_by_key(|s| s.to_ascii_lowercase());
    scenes.dedup();
    scenes
}
