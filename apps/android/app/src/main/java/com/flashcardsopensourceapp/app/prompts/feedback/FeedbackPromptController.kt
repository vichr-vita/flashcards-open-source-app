package com.flashcardsopensourceapp.app.prompts.feedback

import android.content.Context
import com.flashcardsopensourceapp.app.R
import com.flashcardsopensourceapp.core.ui.TransientMessageController
import com.flashcardsopensourceapp.data.local.model.feedback.CloudFeedbackTrigger
import com.flashcardsopensourceapp.data.local.model.feedback.cloudFeedbackMessageMaximumLength
import com.flashcardsopensourceapp.data.local.repository.FeedbackRepository
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class FeedbackPromptUiState(
    val isVisible: Boolean,
    val trigger: CloudFeedbackTrigger,
    val message: String,
    val isSubmitting: Boolean,
    val errorMessage: String?
)

class FeedbackPromptController(
    private val appScope: CoroutineScope,
    context: Context,
    private val feedbackRepository: FeedbackRepository,
    private val promptStore: FeedbackPromptStore,
    private val messageController: TransientMessageController,
    private val feedbackPromptIdentityKeyProvider: () -> FeedbackPromptIdentityKey
) {
    private val applicationContext = context.applicationContext
    private val initialFeedbackPromptIdentityKey = feedbackPromptIdentityKeyProvider()
    private val activeFeedbackPromptIdentityKeyMutable = MutableStateFlow(initialFeedbackPromptIdentityKey)
    private val uiStateMutable = MutableStateFlow(
        FeedbackPromptUiState(
            isVisible = false,
            trigger = CloudFeedbackTrigger.SETTINGS,
            message = promptStore.loadState(identityKey = initialFeedbackPromptIdentityKey).draftMessage,
            isSubmitting = false,
            errorMessage = null
        )
    )

    fun observeUiState(): StateFlow<FeedbackPromptUiState> {
        return uiStateMutable.asStateFlow()
    }

    fun openSettingsFeedback() {
        val currentUiState = uiStateMutable.value
        if (currentUiState.isVisible) {
            return
        }

        val identityKey = feedbackPromptIdentityKeyProvider()
        activeFeedbackPromptIdentityKeyMutable.value = identityKey
        uiStateMutable.value = currentUiState.copy(
            isVisible = true,
            trigger = CloudFeedbackTrigger.SETTINGS,
            message = promptStore.loadState(identityKey = identityKey).draftMessage,
            isSubmitting = false,
            errorMessage = null
        )
    }

    fun updateMessage(message: String) {
        val currentUiState = uiStateMutable.value
        if (currentUiState.isVisible.not() || currentUiState.isSubmitting) {
            return
        }

        promptStore.saveDraftMessage(
            identityKey = activeFeedbackPromptIdentityKeyMutable.value,
            message = message
        )
        uiStateMutable.value = currentUiState.copy(
            message = message,
            errorMessage = null
        )
    }

    fun dismiss() {
        val currentUiState = uiStateMutable.value
        if (currentUiState.isVisible.not() || currentUiState.isSubmitting) {
            return
        }

        promptStore.saveDraftMessage(
            identityKey = activeFeedbackPromptIdentityKeyMutable.value,
            message = currentUiState.message
        )
        uiStateMutable.value = currentUiState.copy(
            isVisible = false,
            errorMessage = null
        )
    }

    fun submit() {
        val currentUiState = uiStateMutable.value
        if (currentUiState.isVisible.not() || currentUiState.isSubmitting) {
            return
        }

        val trimmedMessage = currentUiState.message.trim()
        val validationError = validateFeedbackMessage(message = trimmedMessage)
        if (validationError != null) {
            uiStateMutable.value = currentUiState.copy(errorMessage = validationError)
            return
        }

        uiStateMutable.value = currentUiState.copy(
            message = trimmedMessage,
            isSubmitting = true,
            errorMessage = null
        )
        val identityKey = activeFeedbackPromptIdentityKeyMutable.value
        promptStore.saveDraftMessage(identityKey = identityKey, message = trimmedMessage)

        appScope.launch {
            try {
                val feedbackState = feedbackRepository.submitFeedback(
                    trigger = currentUiState.trigger,
                    message = trimmedMessage
                )
                promptStore.recordFeedbackSubmitted(
                    identityKey = identityKey,
                    feedbackState = feedbackState,
                    nowMillis = System.currentTimeMillis()
                )
                promptStore.clearDraftMessage(identityKey = identityKey)
                uiStateMutable.value = FeedbackPromptUiState(
                    isVisible = false,
                    trigger = CloudFeedbackTrigger.SETTINGS,
                    message = "",
                    isSubmitting = false,
                    errorMessage = null
                )
                messageController.showMessage(
                    message = applicationContext.getString(R.string.feedback_prompt_submit_success)
                )
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                promptStore.saveDraftMessage(identityKey = identityKey, message = trimmedMessage)
                uiStateMutable.update { state ->
                    state.copy(
                        isSubmitting = false,
                        errorMessage = error.message
                            ?: applicationContext.getString(R.string.feedback_prompt_submit_failed)
                    )
                }
            }
        }
    }

    private fun validateFeedbackMessage(message: String): String? {
        if (message.isEmpty()) {
            return applicationContext.getString(R.string.feedback_prompt_empty_message)
        }
        if (message.length > cloudFeedbackMessageMaximumLength) {
            return applicationContext.resources.getQuantityString(
                R.plurals.feedback_prompt_message_too_long,
                cloudFeedbackMessageMaximumLength,
                cloudFeedbackMessageMaximumLength
            )
        }

        return null
    }
}
