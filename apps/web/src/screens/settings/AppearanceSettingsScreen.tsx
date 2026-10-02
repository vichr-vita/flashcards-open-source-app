import { MonitorIcon, MoonIcon, SunIcon } from "lucide-react";
import type { ReactElement } from "react";
import { useI18n } from "../../i18n";
import { setThemePreference, useThemePreference } from "../../theme";
import { SettingsGroup, SettingsShell } from "./SettingsShared";

const themeOptions = [
  { preference: "system", icon: MonitorIcon },
  { preference: "light", icon: SunIcon },
  { preference: "dark", icon: MoonIcon },
] as const;

export function AppearanceSettingsScreen(): ReactElement {
  const { t } = useI18n();
  const preference = useThemePreference();

  return (
    <SettingsShell title={t("appearanceSettings.title")} subtitle={t("appearanceSettings.subtitle")} activeTab="general">
      <SettingsGroup>
        <fieldset className="appearance-options">
          <legend className="sr-only">{t("appearanceSettings.title")}</legend>
          {themeOptions.map(({ preference: option, icon: Icon }) => (
            <label className="appearance-option" key={option}>
              <input
                type="radio"
                name="appearance"
                value={option}
                checked={preference === option}
                onChange={() => setThemePreference(option)}
                data-testid={`appearance-option-${option}`}
              />
              <Icon size={19} strokeWidth={1.8} aria-hidden="true" />
              <span>{t(`appearanceSettings.${option}`)}</span>
            </label>
          ))}
        </fieldset>
        <p className="subtitle">{t("appearanceSettings.systemHint")}</p>
      </SettingsGroup>
    </SettingsShell>
  );
}
