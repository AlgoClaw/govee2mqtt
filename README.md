This is a fork of [wez/govee2mqtt](https://github.com/wez/govee2mqtt) optimized for LAN control of devices.

#### Modifications Inlcude:
1. Generating each "scene" (e.g., "Sunrise", "Easter", etc.) and "sub-scene" (e.g., "Christmas-B", "Poppin-E", etc.) as available "effects" in Home Assistant (and controllable via MQTT, generally).

   - New script [govee_scenes.rs](https://github.com/AlgoClaw/govee2mqtt/blob/main/src/govee_scenes.rs) to parse out the scenes.

   - Existing scripts (e.g., lan_api.rs, lan_control.rs, state.rs) use govee_scenes.rs to parse the scenes (instead of each script repeating the same parsing internally).

   - Removed instances of "...scene_code != 0" as it was causing the scene "Sunrise" to be missing as "Sunrise" has a "sceneCode" of 0.

   - NOT de-duplicating scenes by name. Instead, duplicated names are appended with "(1)", "(2)", etc. As of May 30, 2025, the H7039 has two different scenes each with the same name "Halloween" (this is in addition to also having "Halloween B" and "Halloween C"). Now, both of these are included as "Halloween (1)" and "Halloween (2)"

2. Using the [v1.2 decoding method](https://github.com/AlgoClaw/Govee/blob/main/decoded/v1.2/explanation_v1.2.md) to support more devices.
   - Heavy modification to [ble.rs](https://github.com/AlgoClaw/govee2mqtt/blob/main/src/ble.rs) to integrate this method.

#### TODO / Known Issues:
1. The status of the device (when changed via LAN API) does not update in Home Assistant, is slow to update, or updates to the previous selection. Likely related to [poll_lan_api](https://github.com/AlgoClaw/govee2mqtt/blob/e35d488889a0c13ab32fc2ad2a2154d27d6c59c4/src/service/state.rs#L232) of state.rs.

#### NOTES:
- Nearly all modifications made in this fork were AI generated using Gemini 2.5 Pro (preview) with "Ultra" access.
- Existing functionality for control via cloud services (Govee API (with key) or AWS) remain. However, I do not use the cloud functions and modifications to the code (in this fork) may have broken these integrations. I have no interest in fixing these if they break.
- I am not competent in rust (although I am learning). If I cannot get Gemini to do the thing I want, I am likely not to proceed further.
