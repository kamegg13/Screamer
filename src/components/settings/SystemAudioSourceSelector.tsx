import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";

interface SystemAudioSource {
  id: string;
  name: string;
  is_default: boolean;
}

interface SystemAudioSourceSelectorProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const SystemAudioSourceSelector: React.FC<SystemAudioSourceSelectorProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const [sources, setSources] = useState<SystemAudioSource[]>([]);
    const [selectedSource, setSelectedSource] = useState<string | null>(null);
    const [isLoading, setIsLoading] = useState(true);

    const fetchSources = useCallback(async () => {
      setIsLoading(true);
      try {
        const result = await invoke<SystemAudioSource[]>(
          "get_system_audio_sources",
        );
        setSources(result);
        if (result.length > 0 && selectedSource === null) {
          const defaultSource = result.find((s) => s.is_default);
          setSelectedSource(defaultSource?.id ?? result[0].id);
        }
      } catch (error) {
        console.error("Failed to get system audio sources:", error);
        setSources([]);
      } finally {
        setIsLoading(false);
      }
    }, [selectedSource]);

    useEffect(() => {
      fetchSources();
    }, [fetchSources]);

    const sourceOptions = sources.map((source) => ({
      value: source.id,
      label: source.name,
    }));

    if (sources.length === 0 && !isLoading) {
      return (
        <SettingContainer
          title={t("settings.sound.systemAudio.source")}
          description={t("settings.sound.systemAudio.sourceDescription")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <span className="text-sm text-text/50">
            {t("settings.sound.systemAudio.noSources")}
          </span>
        </SettingContainer>
      );
    }

    return (
      <SettingContainer
        title={t("settings.sound.systemAudio.source")}
        description={t("settings.sound.systemAudio.sourceDescription")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <div className="flex items-center space-x-1">
          <Dropdown
            options={sourceOptions}
            selectedValue={selectedSource}
            onSelect={setSelectedSource}
            placeholder={
              isLoading
                ? t("settings.sound.outputDevice.loading")
                : t("settings.sound.systemAudio.source")
            }
            disabled={isLoading || sources.length === 0}
            onRefresh={fetchSources}
          />
        </div>
      </SettingContainer>
    );
  });
