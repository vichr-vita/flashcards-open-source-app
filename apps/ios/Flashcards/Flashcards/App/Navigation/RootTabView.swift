import Foundation
import SwiftUI

private let rootTabUITestLaunchScenarioEnvironmentKey: String = "FLASHCARDS_UI_TEST_LAUNCH_SCENARIO"

struct RootTabView: View {
    @Environment(\.scenePhase) private var scenePhase
    @Environment(FlashcardsStore.self) private var store: FlashcardsStore
    @Environment(AppNavigationModel.self) private var navigation: AppNavigationModel

    @State private var premiumPresenter: PremiumPresenter = PremiumPresenter()
    private var settingsAttentionSummary: SettingsAttentionSummary {
        makeSettingsAttentionSummary(
            issues: makeSettingsAttentionIssues(cloudState: store.cloudSettings?.cloudState)
        )
    }

    private var reviewReminderAttentionBadgeCount: Int {
        isReviewReminderAttentionVisible(
            state: store.reviewReminderAttentionState,
            workspaceId: store.workspace?.workspaceId
        ) ? 1 : 0
    }

    private var shouldExposeReviewReminderAttentionBadgeMarker: Bool {
        ProcessInfo.processInfo.environment[rootTabUITestLaunchScenarioEnvironmentKey] != nil
    }

    private var accountDeletionSuccessPresentation: Binding<Bool> {
        Binding<Bool>(
            get: {
                store.accountDeletionSuccessMessage != nil
            },
            set: { isPresented in
                if isPresented == false {
                    store.dismissAccountDeletionSuccessMessage()
                }
            }
        )
    }

    private var premiumPresentation: Binding<PremiumPresentationRequest?> {
        Binding(
            get: {
                guard store.feedbackPresentation == nil,
                      store.presentedTechnicalError == nil,
                      store.activeCloudSignInSheetCount == 0 else {
                    return nil
                }
                return self.premiumPresenter.request
            },
            set: { presentation in
                if presentation == nil {
                    self.premiumPresenter.finish(outcome: .dismissed)
                }
            }
        )
    }

    private func clearPremiumPresentationForIdentityChange() {
        self.premiumPresenter.finish(outcome: .identityChanged)
        self.store.aiChatStore.quotaRefusal = nil
    }

    private var feedbackPresentation: Binding<FeedbackPresentation?> {
        Binding<FeedbackPresentation?>(
            get: {
                store.feedbackPresentation
            },
            set: { presentation in
                if presentation == nil {
                    store.dismissFeedbackSheet()
                } else {
                    store.feedbackPresentation = presentation
                }
            }
        )
    }

    private var accountDeletedTitle: String {
        String(
            localized: "root_tab.account_deleted.title",
            table: "Foundation",
            comment: "Account deletion success alert title"
        )
    }

    private var confirmationButtonTitle: String {
        String(
            localized: "shared.ok",
            table: "Foundation",
            comment: "Confirmation button title"
        )
    }

    @MainActor
    private func prepareTabForPresentationIfNeeded(nextTab: AppTab) {
        guard self.store.currentVisibleTab != nextTab else {
            return
        }

        let previousTab = self.store.currentVisibleTab
        prepareVisibleTabForPresentationWithBreadcrumb(
            store: self.store,
            selectedTab: nextTab,
            previousTab: previousTab,
            scenePhase: self.scenePhase,
            isStartupReady: nil,
            isRecoveryGateActive: self.store.cloudCredentialRecoveryState != nil,
            now: Date()
        )
    }

    @MainActor
    private func refreshSelectedTabIfNeeded(nextTab: AppTab) async {
        guard self.store.isCloudSyncBlocked == false else {
            return
        }

        switch nextTab {
        case .review:
            await self.store.refreshReviewBadgesIfNeeded()
        case .progress:
            await self.store.refreshProgressIfNeeded()
        case .ai, .cards, .settings:
            return
        }
    }

    var body: some View {
        Group {
            if let recoveryState = store.cloudCredentialRecoveryState {
                CloudCredentialRecoveryGateView(recoveryState: recoveryState)
                    .environment(store)
                    .overlay {
                        self.uiTestLaunchPreparationStatusMarker
                    }
            } else {
                self.tabRoot
            }
        }
        .onChange(of: store.cloudSettings?.linkedUserId) { _, _ in
            self.clearPremiumPresentationForIdentityChange()
        }
        .onChange(of: store.cloudSettings?.cloudState) { _, _ in
            self.clearPremiumPresentationForIdentityChange()
        }
        .onChange(of: store.cloudEntitlement) { _, entitlement in
            self.premiumPresenter.reconcileAccess(entitlement: entitlement)
        }
    }

    @ViewBuilder
    private var uiTestLaunchPreparationStatusMarker: some View {
        if let uiTestLaunchPreparationValue = store.uiTestLaunchPreparationStatus.accessibilityValue {
            Text("ui-test-launch-preparation-status")
                .font(.system(size: 1))
                .foregroundStyle(.clear)
                .allowsHitTesting(false)
                .accessibilityElement(children: .ignore)
                .accessibilityIdentifier(UITestIdentifier.uiTestLaunchPreparationStatus)
                .accessibilityLabel("UI test launch preparation status")
                .accessibilityValue(uiTestLaunchPreparationValue)
        }
    }

    private var tabRoot: some View {
        self.tabRootAlerts
            .environment(self.premiumPresenter)
    }

    private var tabRootBase: some View {
        @Bindable var navigation = self.navigation
        let selectedTabBinding = Binding<AppTab>(
            get: {
                navigation.selectedTab
            },
            set: { nextTab in
                let previousTab = navigation.selectedTab
                prepareVisibleTabForPresentationWithBreadcrumb(
                    store: self.store,
                    selectedTab: nextTab,
                    previousTab: previousTab,
                    scenePhase: self.scenePhase,
                    isStartupReady: nil,
                    isRecoveryGateActive: self.store.cloudCredentialRecoveryState != nil,
                    now: Date()
                )
                navigation.selectTab(nextTab)
            }
        )

        return TabView(selection: selectedTabBinding) {
            self.reviewTab
            self.progressTab
            self.aiTab
            self.cardsTab
            self.settingsTab(settingsPath: $navigation.settingsPath)
        }
    }

    private var tabRootTasks: some View {
        self.tabRootBase
        .tabBarMinimizeBehavior(.never)
        .task {
            let previousTab = store.currentVisibleTab
            prepareVisibleTabForPresentationWithBreadcrumb(
                store: self.store,
                selectedTab: self.navigation.selectedTab,
                previousTab: previousTab,
                scenePhase: self.scenePhase,
                isStartupReady: nil,
                isRecoveryGateActive: self.store.cloudCredentialRecoveryState != nil,
                now: Date()
            )
        }
        .overlay {
            ZStack {
                GlobalTransientBannerHost()

                if store.accountDeletionState != .hidden {
                    AccountDeletionProgressView()
                        .environment(store)
                }

                self.uiTestLaunchPreparationStatusMarker
            }
        }
    }

    private var tabRootChangeHandlers: some View {
        self.tabRootTasks
        .onChange(of: self.navigation.selectedTab) { _, nextTab in
            self.prepareTabForPresentationIfNeeded(nextTab: nextTab)
            Task { @MainActor in
                await self.refreshSelectedTabIfNeeded(nextTab: nextTab)
            }

            guard usesFastCloudSyncPolling(tab: nextTab) else {
                return
            }

            let triggerSource: CloudSyncTriggerSource = nextTab == .review ? .reviewTabSelected : .cardsTabSelected
            store.triggerCloudSyncIfLinked(
                trigger: CloudSyncTrigger(
                    source: triggerSource,
                    now: Date(),
                    extendsFastPolling: true,
                    allowsVisibleChangeBanner: true,
                    surfacesGlobalErrorMessage: false,
                    capturesTechnicalFailures: false
                )
            )
        }
    }

    private var tabRootSheets: some View {
        self.tabRootChangeHandlers
        .sheet(item: self.premiumPresentation) { request in
            PremiumComingSoon(request: request)
                .environment(store)
                .environment(self.premiumPresenter)
        }
        .sheet(item: self.feedbackPresentation) { presentation in
            FeedbackSheet(presentation: presentation)
                .environment(store)
        }
    }

    private var tabRootAlerts: some View {
        self.tabRootSheets
        .alert(
            self.accountDeletedTitle,
            isPresented: self.accountDeletionSuccessPresentation
        ) {
            Button(
                self.confirmationButtonTitle,
                role: .cancel
            ) {
                store.dismissAccountDeletionSuccessMessage()
            }
        } message: {
            Text(store.accountDeletionSuccessMessage ?? "")
        }
    }

    private var reviewTab: some View {
        NavigationStack {
            ReviewView()
                .overlay(alignment: .topLeading) {
                    self.reviewReminderAttentionBadgeMarker
                }
        }
        .tabItem {
            Label(
                String(
                    localized: "root_tab.review.title",
                    table: "Foundation",
                    comment: "Review tab title"
                ),
                systemImage: "rectangle.on.rectangle"
            )
            .accessibilityIdentifier(UITestIdentifier.rootTabReviewItem)
        }
        .badge(self.reviewReminderAttentionBadgeCount)
        .tag(AppTab.review)
    }

    @ViewBuilder
    private var reviewReminderAttentionBadgeMarker: some View {
        if self.shouldExposeReviewReminderAttentionBadgeMarker && self.reviewReminderAttentionBadgeCount > 0 {
            Color.clear
                .frame(width: 1, height: 1)
                .allowsHitTesting(false)
                .accessibilityElement(children: .ignore)
                .accessibilityIdentifier(UITestIdentifier.rootTabReviewReminderBadge)
                .accessibilityValue(String(self.reviewReminderAttentionBadgeCount))
        }
    }

    private var progressTab: some View {
        NavigationStack {
            ProgressScreen()
        }
        .tabItem {
            Label(
                String(
                    localized: "root_tab.progress.title",
                    defaultValue: "Progress",
                    table: "Foundation",
                    comment: "Progress tab title"
                ),
                systemImage: "chart.bar.xaxis"
            )
            .accessibilityIdentifier(UITestIdentifier.rootTabProgressItem)
        }
        .tag(AppTab.progress)
    }

    private var aiTab: some View {
        NavigationStack {
            AIChatView(chatStore: store.aiChatStore)
        }
        .id(self.navigation.aiTabVisitID)
        .tabItem {
            Label(
                String(
                    localized: "root_tab.ai.title",
                    defaultValue: "AI",
                    table: "Foundation",
                    comment: "AI tab title"
                ),
                systemImage: "sparkles.rectangle.stack"
            )
            .accessibilityIdentifier(UITestIdentifier.rootTabAIItem)
        }
        .tag(AppTab.ai)
    }

    private var cardsTab: some View {
        NavigationStack {
            CardsScreen()
        }
        .tabItem {
            Label(
                String(
                    localized: "root_tab.cards.title",
                    defaultValue: "Cards",
                    table: "Foundation",
                    comment: "Cards tab title"
                ),
                systemImage: "rectangle.stack"
            )
            .accessibilityIdentifier(UITestIdentifier.rootTabCardsItem)
        }
        .tag(AppTab.cards)
    }

    private func settingsTab(settingsPath: Binding<[SettingsNavigationDestination]>) -> some View {
        NavigationStack(path: settingsPath) {
            SettingsView()
                .navigationDestination(for: SettingsNavigationDestination.self) { destination in
                    self.settingsDestinationView(destination: destination)
                }
        }
        .tabItem {
            Label(
                String(
                    localized: "root_tab.settings.title",
                    table: "Foundation",
                    comment: "Settings tab title"
                ),
                systemImage: "gearshape"
            )
            .accessibilityIdentifier(UITestIdentifier.rootTabSettingsItem)
        }
        .badge(self.settingsAttentionSummary.settingsTabCount)
        .tag(AppTab.settings)
    }

    @ViewBuilder
    private func settingsDestinationView(destination: SettingsNavigationDestination) -> some View {
        switch destination {
        case .subscription:
            SubscriptionSettingsView()
        case .currentWorkspace:
            CurrentWorkspaceView()
        case .accentColor:
            AccentColorSettingsView()
        case .reviewAnimations:
            ReviewAnimationsSettingsView()
        case .aiChatSuggestions:
            AIChatSuggestionsSettingsView()
        case .ownOpenAIKey:
            OwnOpenAIKeySettingsView()
        case .leaderboardParticipation:
            LeaderboardParticipationSettingsView()
        case .productAnalytics:
            ProductAnalyticsSettingsView()
        case .language:
            LanguageSettingsView()
        case .feedback:
            FeedbackSettingsView()
        case .device:
            ThisDeviceSettingsView()
        case .access:
            AccessSettingsView()
        case .accessPermissionDetail(let kind):
            AccessPermissionDetailView(kind: kind)
        case .test:
            TestSettingsView()
        case .testAnimations:
            TestAnimationsView()
        case .notificationDiagnostics:
            NotificationDiagnosticsView()
        case .localSyncDiagnostics:
            LocalSyncDiagnosticsView()
        case .notifications:
            NotificationsSettingsView()
        case .workspaceScheduler:
            SchedulerSettingsDetailView()
        case .workspaceExport:
            WorkspaceExportView()
        case .workspaceImport:
            WorkspaceImportView()
        case .workspaceDecks:
            DecksScreen()
        case .workspaceTags:
            TagsScreen()
        case .accountStatus:
            AccountStatusView()
        case .accountLegal:
            AccountLegalView()
        case .accountSupport:
            AccountSupportView()
        case .accountOpenSource:
            AccountOpenSourceView()
        case .accountServer:
            ServerSettingsView()
        case .accountAgentConnections:
            AgentConnectionsView()
        case .accountDangerZone:
            DangerZoneView()
        case .resetStudyProgress:
            ResetStudyProgressView()
        case .deleteCurrentWorkspace:
            DeleteCurrentWorkspaceView()
        }
    }
}

#Preview {
    RootTabView()
        .environment(FlashcardsStore())
        .environment(AppNavigationModel())
}
