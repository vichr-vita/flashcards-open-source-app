import Foundation

@MainActor
extension FlashcardsStore {
    func presentFeedbackSheet(trigger: FeedbackTrigger) {
        self.feedbackPresentation = makeFeedbackPresentation(trigger: trigger)
    }

    func dismissFeedbackSheet() {
        self.feedbackPresentation = nil
    }

    func loadFeedbackDraftMessage() -> String {
        loadFeedbackDraft(
            identityKey: self.feedbackPromptIdentityKey,
            userDefaults: self.userDefaults
        )
    }

    func saveFeedbackDraftMessage(message: String) {
        saveFeedbackDraft(
            identityKey: self.feedbackPromptIdentityKey,
            message: message,
            userDefaults: self.userDefaults
        )
    }

    func submitFeedback(
        trigger: FeedbackTrigger,
        message: String
    ) async throws {
        let trimmedMessage = trimmedFeedbackMessage(message)
        guard trimmedMessage.isEmpty == false else {
            throw LocalStoreError.validation(
                aiSettingsLocalized("feedback.sheet.emptyMessageError", "Write feedback before sending.")
            )
        }
        guard trimmedMessage.count <= feedbackMessageMaximumCharacters else {
            throw LocalStoreError.validation(
                aiSettingsLocalizedFormat(
                    "feedback.sheet.messageTooLong",
                    "Keep feedback under %d characters.",
                    feedbackMessageMaximumCharacters
                )
            )
        }

        let now = Date()
        let request = makeFeedbackSubmissionRequest(
            workspaceId: self.workspace?.workspaceId,
            installationId: self.cloudSettings?.installationId,
            trigger: trigger,
            message: trimmedMessage,
            now: now
        )
        let feedbackState = try await self.withFeedbackCloudSession { cloudSyncService, session in
            try await cloudSyncService.submitFeedback(
                apiBaseUrl: session.apiBaseUrl,
                authorizationHeader: session.authorizationHeaderValue,
                request: request
            )
        }

        self.updateFeedbackPromptState(
            state: makeFeedbackPromptStateAfterSubmission(
                promptState: self.feedbackPromptState,
                feedbackState: feedbackState,
                submittedAt: Date()
            )
        )
        clearFeedbackDraft(
            identityKey: self.feedbackPromptIdentityKey,
            userDefaults: self.userDefaults
        )
        self.dismissFeedbackSheet()
        self.enqueueTransientBanner(banner: makeFeedbackSentBanner())
    }

    func clearFeedbackPromptState() {
        self.feedbackPresentation = nil
        self.feedbackPromptState = makeDefaultFeedbackPromptState()
        clearFeedbackPromptPersistence(
            identityKey: self.feedbackPromptIdentityKey,
            userDefaults: self.userDefaults
        )
    }

    func reloadFeedbackPromptStateForCurrentIdentity() {
        self.feedbackPromptState = loadFeedbackPromptState(
            identityKey: self.feedbackPromptIdentityKey,
            userDefaults: self.userDefaults,
            decoder: self.decoder
        )
    }

    private func withFeedbackCloudSession<Result>(
        operation: @MainActor (any CloudSyncServing, CloudLinkedSession) async throws -> Result
    ) async throws -> Result {
        let cloudSyncService = try requireCloudSyncService(cloudSyncService: self.dependencies.cloudSyncService)
        let session = try await self.cloudSessionForFeedback()
        return try await self.withCloudSessionPreservingStableContext(linkedSession: session) { refreshedSession in
            try await operation(cloudSyncService, refreshedSession)
        }
    }

    private func cloudSessionForFeedback() async throws -> CloudLinkedSession {
        try self.throwIfCloudCredentialRecoveryRequired()
        if case .blocked(let message) = self.syncStatus {
            throw LocalStoreError.validation(message)
        }
        if self.cloudSettings?.cloudState == .linked {
            if self.cloudRuntime.activeCloudSession() == nil {
                try await self.restoreCloudLinkFromStoredCredentials(trigger: self.manualCloudSyncTrigger(now: Date()))
            }

            return try await self.withAuthenticatedCloudSession { session in
                session
            }
        }

        let restoredGuestSession = try await self.restoreGuestCloudSessionIfNeeded(
            trigger: self.manualCloudSyncTrigger(now: Date())
        )
        return restoredGuestSession.session
    }

    private func updateFeedbackPromptState(state: PersistedFeedbackPromptState) {
        self.feedbackPromptState = state
        saveFeedbackPromptState(
            identityKey: self.feedbackPromptIdentityKey,
            state: state,
            userDefaults: self.userDefaults,
            encoder: self.encoder
        )
    }

    private var feedbackPromptIdentityKey: FeedbackPromptIdentityKey {
        makeFeedbackPromptIdentityKey(cloudSettings: self.cloudSettings)
    }
}
