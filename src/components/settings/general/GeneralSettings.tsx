import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { MicrophoneSelector } from "../MicrophoneSelector";
import { ShortcutInput } from "../ShortcutInput";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { OutputDeviceSelector } from "../OutputDeviceSelector";
import { PushToTalk } from "../PushToTalk";
import { AudioFeedback } from "../AudioFeedback";
import { useSettings } from "../../../hooks/useSettings";
import { VolumeSlider } from "../VolumeSlider";
import { MuteWhileRecording } from "../MuteWhileRecording";
import { ModelSettingsCard } from "./ModelSettingsCard";
import { LongAudioModelSettings } from "./LongAudioModelSettings";
import { SystemAudioToggle } from "../SystemAudioToggle";
import { SystemAudioGainSlider } from "../SystemAudioGainSlider";
import { SystemAudioSourceSelector } from "../SystemAudioSourceSelector";

export const GeneralSettings: React.FC = () => {
  const { t } = useTranslation();
  const { audioFeedbackEnabled } = useSettings();
  const [systemAudioEnabled, setSystemAudioEnabled] = useState(false);

  useEffect(() => {
    let cancelled = false;
    invoke<boolean>("get_system_audio_enabled")
      .then((value) => {
        if (!cancelled) {
          setSystemAudioEnabled(value);
        }
      })
      .catch((error) => {
        console.error("Failed to get system audio enabled state:", error);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.general.title")}>
        <ShortcutInput shortcutId="transcribe" grouped={true} />
        <ShortcutInput shortcutId="cancel" grouped={true} />
        <ShortcutInput shortcutId="pause" grouped={true} />
        <ShortcutInput shortcutId="show_history" grouped={true} />
        <ShortcutInput shortcutId="copy_latest_history" grouped={true} />
        <PushToTalk descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
      <ModelSettingsCard />
      <LongAudioModelSettings />
      <SettingsGroup title={t("settings.sound.title")}>
        <MicrophoneSelector descriptionMode="tooltip" grouped={true} />
        <MuteWhileRecording descriptionMode="tooltip" grouped={true} />
        <AudioFeedback descriptionMode="tooltip" grouped={true} />
        <OutputDeviceSelector
          descriptionMode="tooltip"
          grouped={true}
          disabled={!audioFeedbackEnabled}
        />
        <VolumeSlider disabled={!audioFeedbackEnabled} />
        <SystemAudioToggle
          descriptionMode="tooltip"
          grouped={true}
          onToggle={setSystemAudioEnabled}
        />
        {systemAudioEnabled && (
          <>
            <SystemAudioGainSlider descriptionMode="tooltip" grouped={true} />
            <SystemAudioSourceSelector
              descriptionMode="tooltip"
              grouped={true}
            />
          </>
        )}
      </SettingsGroup>
    </div>
  );
};
