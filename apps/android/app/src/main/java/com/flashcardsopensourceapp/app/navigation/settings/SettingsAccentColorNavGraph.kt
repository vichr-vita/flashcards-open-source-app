package com.flashcardsopensourceapp.app.navigation.settings

import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.NavGraphBuilder
import androidx.navigation.NavHostController
import androidx.navigation.compose.composable
import com.flashcardsopensourceapp.app.di.AppGraph
import com.flashcardsopensourceapp.feature.settings.accent.AccentColorRoute
import com.flashcardsopensourceapp.feature.settings.accent.AccentColorViewModel

internal fun NavGraphBuilder.registerAccentColorDestination(
    appGraph: AppGraph,
    navController: NavHostController
) {
    composable(route = SettingsAccentColorDestination.route) {
        val accentColorViewModel = viewModel<AccentColorViewModel>(
            viewModelStoreOwner = appGraph.accentColorViewModelStoreOwner,
            factory = appGraph.accentColorViewModelFactory
        )
        val uiState by accentColorViewModel.uiState.collectAsStateWithLifecycle()
        key(uiState.identityKey) {
            AccentColorRoute(
                uiState = uiState,
                onSelectColor = { color ->
                    accentColorViewModel.selectColor(color = color, identityKey = uiState.identityKey)
                },
                onBack = { navController.popBackStack() }
            )
        }
    }
}
