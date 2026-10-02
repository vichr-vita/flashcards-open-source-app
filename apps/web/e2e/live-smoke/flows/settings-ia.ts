import { expect } from "@playwright/test";

import {
  trackedClick,
  trackedExpectVisible,
  trackedWaitForUrl,
} from "../../live-smoke.actions";
import {
  accountStatusRoute,
  reviewRoute,
  settingsAppearanceRoute,
  settingsAccessRoute,
  settingsCurrentWorkspaceRoute,
  settingsDeviceRoute,
  settingsLanguageRoute,
  settingsNotificationsRoute,
  settingsSchedulerRoute,
  settingsServerRoute,
  workspaceRoutePrefix,
} from "../../../src/routes";
import { localUiTimeoutMs } from "../config";
import { primaryNavigationLinkSelector } from "../navigation";
import { runLiveSmokeStep } from "../steps";
import type { LiveSmokeSession } from "../types";

type SettingsDetailTarget = Readonly<{
  rowTestId: string;
  route: string;
  actionName: string;
}>;

const settingsDetailTargets: ReadonlyArray<SettingsDetailTarget> = [
  {
    rowTestId: "settings-row-account-status",
    route: accountStatusRoute,
    actionName: "open Account status settings",
  },
  {
    rowTestId: "settings-row-current-workspace",
    route: settingsCurrentWorkspaceRoute,
    actionName: "open Workspace settings",
  },
  {
    rowTestId: "settings-row-appearance",
    route: settingsAppearanceRoute,
    actionName: "open Appearance settings",
  },
  {
    rowTestId: "settings-row-language",
    route: settingsLanguageRoute,
    actionName: "open Language settings",
  },
  {
    rowTestId: "settings-row-review-reminders",
    route: settingsNotificationsRoute,
    actionName: "open Notifications settings",
  },
  {
    rowTestId: "settings-row-access",
    route: settingsAccessRoute,
    actionName: "open Access settings",
  },
  {
    rowTestId: "settings-row-scheduling",
    route: settingsSchedulerRoute,
    actionName: "open Scheduling / FSRS settings",
  },
  {
    rowTestId: "settings-row-server",
    route: settingsServerRoute,
    actionName: "open Server settings",
  },
  {
    rowTestId: "settings-row-device-diagnostics",
    route: settingsDeviceRoute,
    actionName: "open Device settings",
  },
];

const rootRowTestIds: ReadonlyArray<string> = [
  "settings-row-review-app-store",
  "settings-row-private-feedback",
  "settings-row-account-status",
  "settings-row-current-workspace",
  "settings-row-review-reminders",
  "settings-row-appearance",
  "settings-row-language",
  "settings-row-access",
  "settings-row-decks",
  "settings-row-tags",
  "settings-row-import",
  "settings-row-export",
  "settings-row-feedback",
  "settings-row-open-source",
  "settings-row-scheduling",
  "settings-row-agent-connections",
  "settings-row-server",
  "settings-row-device-diagnostics",
  "settings-row-reset-study-progress",
  "settings-row-delete-current-workspace",
  "settings-row-delete-account",
];

export async function runSettingsIaFlow(session: LiveSmokeSession): Promise<void> {
  await runLiveSmokeStep(session, "verify Settings first-level IA and detail navigation", async () => {
    await assertSettingsRootTree(session);

    for (const target of settingsDetailTargets) {
      await openSettingsDetailFromRoot(session, target);
    }

    await verifyAppearanceAndLogo(session);
  });
}

async function verifyAppearanceAndLogo(session: LiveSmokeSession): Promise<void> {
  const { page, diagnostics, baseUrl } = session;
  await openSettingsRoot(session, "return to Settings before theme check");
  await trackedClick(diagnostics, "open Appearance", page.getByTestId("settings-row-appearance"));

  for (const option of ["system", "light", "dark"] as const) {
    await expect(page.getByTestId(`appearance-option-${option}`).locator("xpath=..").locator("svg")).toHaveCount(1);
  }

  await page.getByTestId("appearance-option-light").check();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.reload();
  await expect(page.getByTestId("appearance-option-light")).toBeChecked();

  await page.getByTestId("appearance-option-dark").check();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  await page.emulateMedia({ colorScheme: "light" });
  await page.getByTestId("appearance-option-system").check();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.emulateMedia({ colorScheme: "no-preference" });

  await trackedClick(diagnostics, "return to Review through the logo", page.locator(".topbar-brand"));
  await trackedWaitForUrl(page, diagnostics, "confirm logo leads to Review", buildRouteUrlPattern(baseUrl, reviewRoute), localUiTimeoutMs);
}

async function assertSettingsRootTree(session: LiveSmokeSession): Promise<void> {
  const { page, diagnostics } = session;
  await openSettingsRoot(session, "open Settings for IA verification");

  for (const groupLabel of ["Feedback", "Account", "General", "Support", "Advanced"]) {
    await trackedExpectVisible(
      diagnostics,
      `confirm Settings group ${groupLabel} is visible`,
      page.getByRole("heading", { name: groupLabel, exact: true }),
      localUiTimeoutMs,
    );
  }

  for (const rowTestId of rootRowTestIds) {
    await trackedExpectVisible(
      diagnostics,
      `confirm Settings row ${rowTestId} is visible`,
      page.getByTestId(rowTestId),
      localUiTimeoutMs,
    );
  }

  await diagnostics.runAction("confirm Settings Test row is hidden when test mode is off", async () => {
    await expect(page.getByTestId("settings-row-test")).toHaveCount(0);
  });
}

async function openSettingsDetailFromRoot(
  session: LiveSmokeSession,
  target: SettingsDetailTarget,
): Promise<void> {
  const { page, diagnostics, baseUrl } = session;
  await openSettingsRoot(session, `return to Settings before ${target.actionName}`);
  await trackedClick(diagnostics, target.actionName, page.getByTestId(target.rowTestId));
  await trackedWaitForUrl(
    page,
    diagnostics,
    `confirm route after ${target.actionName}`,
    buildRouteUrlPattern(baseUrl, target.route),
    localUiTimeoutMs,
  );
  await trackedExpectVisible(
    diagnostics,
    `confirm settings panel is visible after ${target.actionName}`,
    page.locator(".settings-panel"),
    localUiTimeoutMs,
  );
}

async function openSettingsRoot(session: LiveSmokeSession, actionName: string): Promise<void> {
  const { page, diagnostics, baseUrl } = session;
  await trackedClick(diagnostics, actionName, page.locator(primaryNavigationLinkSelector("/settings")).first());
  await trackedWaitForUrl(
    page,
    diagnostics,
    `${actionName} route`,
    buildRouteUrlPattern(baseUrl, "/settings"),
    localUiTimeoutMs,
  );
}

/**
 * The workspace segment is optional because the flat address reaches the same route through the
 * permanent redirect in `App.tsx`, which external bookmarks still arrive on.
 */
function buildRouteUrlPattern(baseUrl: string, route: string): RegExp {
  const workspaceSegment = `(?:${escapeRegExp(`${workspaceRoutePrefix}/`)}[^/]+)?`;
  return new RegExp(`^${escapeRegExp(baseUrl)}${workspaceSegment}${escapeRegExp(route)}(?:[?#].*)?$`);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
