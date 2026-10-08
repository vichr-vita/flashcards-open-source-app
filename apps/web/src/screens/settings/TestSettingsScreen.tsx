import { useCallback, useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import { usePremiumPresenter } from "../../premium/PremiumProvider";
import { useAppData } from "../../appData";
import {
  markIndexedDbOpenRecoveryFailureAndCheckActive,
  useAppErrorDialog,
} from "../../appError/AppErrorContext";
import { webReviewMobilePromptStoreLinks } from "../../appPlatformLinks";
import { type TranslationKey, type TranslationValues, useI18n } from "../../i18n";
import {
  settingsTestAnimationsRoute,
  settingsTestAppPlatformLinksRoute,
  settingsTestCatalogImportSuccessRoute,
  settingsTestLocalSyncDiagnosticsRoute,
} from "../../routes";
import { useWorkspacePath } from "../../useWorkspacePath";
import {
  loadLocalSyncDiagnosticsReport,
  type LocalSyncDiagnosticsReport,
} from "../../localDb/diagnostics/localSyncDiagnostics";
import { MobileAppPromotionDialog } from "../review/mobileAppPromo/MobileAppPromotionDialog";
import { reviewReactionRatings } from "../review/reactions/reviewReaction";
import { useReviewRatingReactions } from "../review/reactions/useReviewRatingReactions";
import { ReviewRatingReactionLayer } from "../review/reactions/ReviewRatingReactionLayer";
import { SettingsActionCard, SettingsGroup, SettingsNavigationCard, SettingsShell } from "./SettingsShared";

type Translate = (key: TranslationKey, values?: TranslationValues) => string;
type FormatNumber = (value: number, options?: Readonly<Intl.NumberFormatOptions>) => string;
type DiagnosticDisplayValue = string | number | boolean | null;

type DiagnosticField = Readonly<{
  labelKey: TranslationKey;
  value: DiagnosticDisplayValue;
}>;

type DiagnosticFieldGridProps = Readonly<{
  fields: ReadonlyArray<DiagnosticField>;
  formatNumber: FormatNumber;
  t: Translate;
}>;

type ProblemRecordSectionProps = Readonly<{
  title: string;
  isEmpty: boolean;
  emptyMessage: string;
  children: ReactNode;
}>;

export function TestSettingsScreen(): ReactElement {
  const presentPremium = usePremiumPresenter();
  const { t } = useI18n();
  const workspacePath = useWorkspacePath();
  const { showTechnicalErrorPreview } = useAppErrorDialog();
  const [isMobileAppPromotionDialogOpen, setIsMobileAppPromotionDialogOpen] = useState<boolean>(false);

  return (
    <SettingsShell
      title={t("settingsTest.title")}
      subtitle={t("settingsTest.subtitle")}
      activeTab="test"
    >
      <div data-testid="test-settings-screen">
        <SettingsGroup title={t("settingsTest.toolsGroupTitle")}>
          <div className="settings-nav-list">
            <SettingsActionCard
              title={t("premium.previewOffer")}
              description={t("premium.unavailable")}
              value={null}
              onClick={() => { presentPremium?.({ reason: "offer" }); }}
              testId="test-settings-premium-offer"
            />
            <SettingsActionCard
              title={t("premium.previewAi")}
              description={t("premium.retryManually")}
              value={null}
              onClick={() => { presentPremium?.({ reason: "ai-limit", aiUsage: null }); }}
              testId="test-settings-premium-ai-limit"
            />
            <SettingsNavigationCard
              title={t("settingsTest.animations.title")}
              description={t("settingsTest.animations.description")}
              value={t("settingsTest.animations.value")}
              to={workspacePath(settingsTestAnimationsRoute)}
              testId="test-settings-animations-row"
            />
            <SettingsNavigationCard
              title={t("settingsTest.localSyncDiagnostics.title")}
              description={t("settingsTest.localSyncDiagnostics.description")}
              value={t("settingsTest.localSyncDiagnostics.value")}
              to={workspacePath(settingsTestLocalSyncDiagnosticsRoute)}
              testId="test-settings-local-sync-diagnostics-row"
            />
            <SettingsNavigationCard
              title={t("settingsTest.appPlatformLinks.title")}
              description={t("settingsTest.appPlatformLinks.description")}
              value={t("settingsTest.appPlatformLinks.value")}
              to={workspacePath(settingsTestAppPlatformLinksRoute)}
              testId="test-settings-app-platform-links-row"
            />
            <SettingsNavigationCard
              title={t("settingsTest.catalogImportSuccess.title")}
              description={t("settingsTest.catalogImportSuccess.description")}
              value={t("settingsTest.catalogImportSuccess.value")}
              to={workspacePath(settingsTestCatalogImportSuccessRoute)}
              testId="test-settings-catalog-import-success-row"
            />
            <SettingsActionCard
              title={t("settingsTest.technicalError.title")}
              description={t("settingsTest.technicalError.description")}
              value={t("settingsTest.technicalError.value")}
              onClick={showTechnicalErrorPreview}
              testId="test-settings-technical-error-row"
            />
            <SettingsActionCard
              title={t("settingsTest.mobileAppPromo.title")}
              description={t("settingsTest.mobileAppPromo.description")}
              value={t("settingsTest.mobileAppPromo.value")}
              onClick={() => setIsMobileAppPromotionDialogOpen(true)}
              testId="test-settings-mobile-app-promo-row"
            />
          </div>
        </SettingsGroup>
      </div>
      <MobileAppPromotionDialog
        isOpen={isMobileAppPromotionDialogOpen}
        onDismiss={() => setIsMobileAppPromotionDialogOpen(false)}
        storeLinks={webReviewMobilePromptStoreLinks}
      />
    </SettingsShell>
  );
}

function formatDiagnosticDisplayValue(
  value: DiagnosticDisplayValue,
  formatNumber: FormatNumber,
  t: Translate,
): string {
  if (value === null) {
    return t("settingsTest.localSyncDiagnostics.unavailable");
  }

  if (typeof value === "number") {
    return formatNumber(value);
  }

  if (typeof value === "boolean") {
    return String(value);
  }

  return value.trim() === "" ? t("settingsTest.localSyncDiagnostics.unavailable") : value;
}

function DiagnosticFieldGrid(props: DiagnosticFieldGridProps): ReactElement {
  const { fields, formatNumber, t } = props;

  return (
    <dl className="settings-diagnostics-field-grid">
      {fields.map((field) => (
        <div className="settings-diagnostics-field" key={field.labelKey}>
          <dt>{t(field.labelKey)}</dt>
          <dd>{formatDiagnosticDisplayValue(field.value, formatNumber, t)}</dd>
        </div>
      ))}
    </dl>
  );
}

function ProblemRecordSection(props: ProblemRecordSectionProps): ReactElement {
  const { title, isEmpty, emptyMessage, children } = props;

  return (
    <section className="settings-diagnostics-problem-section">
      <h3 className="settings-diagnostics-problem-title">{title}</h3>
      {isEmpty ? <p className="subtitle settings-diagnostics-empty">{emptyMessage}</p> : children}
    </section>
  );
}

function buildCardsSyncFields(report: LocalSyncDiagnosticsReport): ReadonlyArray<DiagnosticField> {
  return [
    { labelKey: "settingsTest.localSyncDiagnostics.fields.workspaceId", value: report.cardsSync.workspaceId },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.installationId", value: report.cardsSync.installationId },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.cloudState", value: report.cardsSync.cloudState },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.localActiveCards", value: report.cardsSync.localActiveCards },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.localDeletedCards", value: report.cardsSync.localDeletedCards },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.pendingCardOperations", value: report.cardsSync.pendingCardOperations },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.failedCardOperations", value: report.cardsSync.failedCardOperations },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.oldestPendingCardOperation", value: report.cardsSync.oldestPendingCardOperation },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.latestCardSyncSuccess", value: report.cardsSync.latestCardSyncSuccess },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.hotStateHydrated", value: report.cardsSync.hotStateHydrated },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.hotCursor", value: report.cardsSync.hotCursor },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.reviewCursor", value: report.cardsSync.reviewCursor },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.latestSyncError", value: report.cardsSync.latestSyncError },
  ];
}

function buildManagedMediaSyncFields(report: LocalSyncDiagnosticsReport): ReadonlyArray<DiagnosticField> {
  return [
    { labelKey: "settingsTest.localSyncDiagnostics.fields.localActiveMediaAssets", value: report.managedMediaSync.localActiveMediaAssets },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.deletedMediaAssets", value: report.managedMediaSync.deletedMediaAssets },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.localMediaBlobs", value: report.managedMediaSync.localMediaBlobs },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.localMediaBytes", value: report.managedMediaSync.localMediaBytes },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.referencedMediaInCards", value: report.managedMediaSync.referencedMediaInCards },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.referencesMissingLocalAsset", value: report.managedMediaSync.referencesMissingLocalAsset },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.assetsMissingLocalBlob", value: report.managedMediaSync.assetsMissingLocalBlob },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.pendingMediaUploads", value: report.managedMediaSync.pendingMediaUploads },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.failedMediaUploads", value: report.managedMediaSync.failedMediaUploads },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.pendingMediaDownloads", value: report.managedMediaSync.pendingMediaDownloads },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.failedMediaDownloads", value: report.managedMediaSync.failedMediaDownloads },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.oldestPendingMediaTransfer", value: report.managedMediaSync.oldestPendingMediaTransfer },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.latestMediaUploadSuccess", value: report.managedMediaSync.latestMediaUploadSuccess },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.latestMediaDownloadCacheSuccess", value: report.managedMediaSync.latestMediaDownloadCacheSuccess },
    { labelKey: "settingsTest.localSyncDiagnostics.fields.latestMediaTransferError", value: report.managedMediaSync.latestMediaTransferError },
  ];
}

export function TestLocalSyncDiagnosticsScreen(): ReactElement {
  const { activeWorkspace } = useAppData();
  const { indexedDbOpenRecoveryState, showCapturedTechnicalError } = useAppErrorDialog();
  const { t, formatNumber } = useI18n();
  const activeWorkspaceId = activeWorkspace?.workspaceId ?? null;
  const isMountedRef = useRef<boolean>(false);
  const [report, setReport] = useState<LocalSyncDiagnosticsReport | null>(null);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const [loadErrorMessage, setLoadErrorMessage] = useState<string>("");
  const [copyStatusMessage, setCopyStatusMessage] = useState<string>("");
  const technicalErrorMessage = t("appError.technicalError.message");

  const refreshDiagnostics = useCallback(async (): Promise<void> => {
    if (indexedDbOpenRecoveryState.hasFailed()) {
      return;
    }

    if (activeWorkspaceId === null) {
      setReport(null);
      setLoadErrorMessage(t("settingsTest.localSyncDiagnostics.noWorkspace"));
      setIsLoading(false);
      return;
    }

    setIsLoading(true);
    setLoadErrorMessage("");
    setCopyStatusMessage("");

    try {
      const nextReport = await loadLocalSyncDiagnosticsReport(activeWorkspaceId);
      indexedDbOpenRecoveryState.throwIfFailed();
      if (isMountedRef.current === false) {
        return;
      }

      setReport(nextReport);
    } catch (error) {
      if (markIndexedDbOpenRecoveryFailureAndCheckActive(indexedDbOpenRecoveryState, error)) {
        return;
      }
      if (isMountedRef.current === false) {
        return;
      }

      showCapturedTechnicalError(error);
      setReport(null);
      setLoadErrorMessage(technicalErrorMessage);
    } finally {
      if (isMountedRef.current && indexedDbOpenRecoveryState.hasFailed() === false) {
        setIsLoading(false);
      }
    }
  }, [activeWorkspaceId, indexedDbOpenRecoveryState, showCapturedTechnicalError, t, technicalErrorMessage]);

  useEffect(() => {
    isMountedRef.current = true;

    return (): void => {
      isMountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    void refreshDiagnostics();
  }, [refreshDiagnostics]);

  async function copyDiagnostics(): Promise<void> {
    if (report === null) {
      return;
    }

    setCopyStatusMessage("");

    if (typeof navigator.clipboard?.writeText !== "function") {
      setCopyStatusMessage(t("settingsTest.localSyncDiagnostics.clipboardUnavailable"));
      return;
    }

    try {
      await navigator.clipboard.writeText(JSON.stringify(report, null, 2));
      setCopyStatusMessage(t("settingsTest.localSyncDiagnostics.copied"));
    } catch {
      setCopyStatusMessage(t("settingsTest.localSyncDiagnostics.copyFailed"));
    }
  }

  return (
    <SettingsShell
      title={t("settingsTest.localSyncDiagnostics.screenTitle")}
      subtitle={t("settingsTest.localSyncDiagnostics.screenSubtitle")}
      activeTab="test"
      panelClassName="settings-panel-test-local-sync-diagnostics"
    >
      <div className="settings-diagnostics-screen" data-testid="test-local-sync-diagnostics-screen">
        <div className="settings-diagnostics-toolbar">
          <p className="subtitle">
            {t("settingsTest.localSyncDiagnostics.generatedAt", {
              value: report?.generatedAt ?? t("settingsTest.localSyncDiagnostics.unavailable"),
            })}
          </p>
          <div className="screen-actions settings-diagnostics-actions">
            <button
              className="ghost-btn"
              type="button"
              disabled={isLoading}
              onClick={() => void refreshDiagnostics()}
              data-testid="local-sync-diagnostics-refresh-button"
            >
              {isLoading ? t("settingsTest.localSyncDiagnostics.refreshing") : t("settingsTest.localSyncDiagnostics.refresh")}
            </button>
            <button
              className="primary-btn"
              type="button"
              disabled={report === null}
              onClick={() => void copyDiagnostics()}
              data-testid="local-sync-diagnostics-copy-button"
            >
              {t("settingsTest.localSyncDiagnostics.copy")}
            </button>
          </div>
        </div>

        {loadErrorMessage === "" ? null : <p className="error-banner">{loadErrorMessage}</p>}
        {copyStatusMessage === "" ? null : (
          <p className="settings-diagnostics-copy-status" aria-live="polite" data-testid="local-sync-diagnostics-copy-status">
            {copyStatusMessage}
          </p>
        )}

        {report === null ? (
          <section className="panel panel-center state-panel settings-diagnostics-state">
            <p className="subtitle">
              {isLoading ? t("settingsTest.localSyncDiagnostics.loading") : t("settingsTest.localSyncDiagnostics.unavailable")}
            </p>
          </section>
        ) : (
          <>
            <SettingsGroup title={t("settingsTest.localSyncDiagnostics.sections.cardsSync")}>
              <DiagnosticFieldGrid fields={buildCardsSyncFields(report)} formatNumber={formatNumber} t={t} />
            </SettingsGroup>

            <SettingsGroup title={t("settingsTest.localSyncDiagnostics.sections.managedMediaSync")}>
              <DiagnosticFieldGrid fields={buildManagedMediaSyncFields(report)} formatNumber={formatNumber} t={t} />
            </SettingsGroup>

            <SettingsGroup title={t("settingsTest.localSyncDiagnostics.sections.problemRecords")}>
              <div className="settings-diagnostics-problem-list">
                <ProblemRecordSection
                  title={t("settingsTest.localSyncDiagnostics.problem.failedCardOutboxOperations")}
                  isEmpty={report.problemRecords.failedCardOutboxOperations.length === 0}
                  emptyMessage={t("settingsTest.localSyncDiagnostics.problem.empty")}
                >
                  {report.problemRecords.failedCardOutboxOperations.map((record) => (
                    <div className="settings-diagnostics-problem-record content-card" key={record.operationId}>
                      <code>{record.operationId}</code>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.cardId")}: <code>{record.cardId}</code></span>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.attemptCount")}: {formatNumber(record.attemptCount)}</span>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.createdAt")}: <code>{record.createdAt}</code></span>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.error")}: <code>{record.lastError}</code></span>
                    </div>
                  ))}
                </ProblemRecordSection>

                <ProblemRecordSection
                  title={t("settingsTest.localSyncDiagnostics.problem.failedMediaTransfers")}
                  isEmpty={report.problemRecords.failedMediaTransfers.length === 0}
                  emptyMessage={t("settingsTest.localSyncDiagnostics.problem.empty")}
                >
                  {report.problemRecords.failedMediaTransfers.map((record) => (
                    <div className="settings-diagnostics-problem-record content-card" key={record.transferId}>
                      <code>{record.transferId}</code>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.mediaAssetId")}: <code>{record.mediaAssetId}</code></span>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.kind")}: <code>{record.kind}</code></span>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.attemptCount")}: {formatNumber(record.attemptCount)}</span>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.updatedAt")}: <code>{record.updatedAt}</code></span>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.error")}: <code>{record.lastError}</code></span>
                    </div>
                  ))}
                </ProblemRecordSection>

                <ProblemRecordSection
                  title={t("settingsTest.localSyncDiagnostics.problem.missingMediaReferences")}
                  isEmpty={report.problemRecords.missingMediaReferences.length === 0}
                  emptyMessage={t("settingsTest.localSyncDiagnostics.problem.empty")}
                >
                  {report.problemRecords.missingMediaReferences.map((record) => (
                    <div className="settings-diagnostics-problem-record content-card" key={record.mediaAssetId}>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.mediaAssetId")}: <code>{record.mediaAssetId}</code></span>
                    </div>
                  ))}
                </ProblemRecordSection>

                <ProblemRecordSection
                  title={t("settingsTest.localSyncDiagnostics.problem.assetsMissingLocalBlob")}
                  isEmpty={report.problemRecords.assetsMissingLocalBlob.length === 0}
                  emptyMessage={t("settingsTest.localSyncDiagnostics.problem.empty")}
                >
                  {report.problemRecords.assetsMissingLocalBlob.map((record) => (
                    <div className="settings-diagnostics-problem-record content-card" key={record.mediaAssetId}>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.mediaAssetId")}: <code>{record.mediaAssetId}</code></span>
                      <span>{t("settingsTest.localSyncDiagnostics.problem.sha256")}: <code>{record.sha256}</code></span>
                    </div>
                  ))}
                </ProblemRecordSection>
              </div>
            </SettingsGroup>
          </>
        )}
      </div>
    </SettingsShell>
  );
}

export function TestAnimationsScreen(): ReactElement {
  const { t } = useI18n();
  const { events, emitReaction, dismissReactions } = useReviewRatingReactions({ reviewReactionAnimationsEnabled: true });

  return (
    <SettingsShell
      title={t("settingsTest.animations.screenTitle")}
      subtitle={t("settingsTest.animations.screenSubtitle")}
      activeTab="test"
      panelClassName="settings-panel-test-animations"
    >
      <div className="settings-test-animation-list review-card-reaction-frame" data-testid="test-animations-screen" onPointerDownCapture={dismissReactions}>
        {reviewReactionRatings.map((rating) => (
          <SettingsGroup key={rating} title={t(`reviewScreen.ratings.${rating}`)}>
            <div className="settings-test-animation-rows">
              <button
                className="settings-test-animation-row content-card"
                type="button"
                data-review-reaction-rating={rating}
                data-testid="test-animation-row"
                onClick={() => emitReaction(rating === "hard" ? 1 : rating === "good" ? 2 : 3)}
              >
                <span className="settings-test-animation-name">{t(`reviewScreen.ratings.${rating}`)}</span>
              </button>
            </div>
          </SettingsGroup>
        ))}
        <ReviewRatingReactionLayer events={events} />
      </div>
    </SettingsShell>
  );
}
