package com.skadi.app

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.skadi.core.SkadiApi
import kotlinx.coroutines.launch

/**
 * Change your own password (SKADI-T-0626).
 *
 * Available to every account, whatever the role: a password the operator minted
 * and read out over the dinner table is one the person should be able to
 * replace with something only they know. The server signs their *other* devices
 * out and leaves this one signed in, which the dialog says plainly — otherwise
 * "why did my tablet log out" is a mystery rather than the point.
 */
@Composable
fun PasswordDialog(api: SkadiApi, onDismiss: () -> Unit) {
    val scope = rememberCoroutineScope()
    var current by remember { mutableStateOf("") }
    var next by remember { mutableStateOf("") }
    var confirm by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var done by remember { mutableStateOf(false) }

    fun submit() {
        if (next != confirm) { error = "Those two don't match."; return }
        if (next.trim().length < 6) { error = "Use at least 6 characters."; return }
        busy = true
        error = null
        scope.launch {
            runCatching { api.changePassword(current, next.trim()) }
                .onSuccess { ok ->
                    busy = false
                    if (ok) done = true else error = "That isn't your current password."
                }
                .onFailure { busy = false; error = it.message ?: "Couldn't change it." }
        }
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (done) "Password changed" else "Change your password") },
        text = {
            if (done) {
                Text("Your other devices have been signed out. This one stays signed in.")
            } else {
                Column(modifier = Modifier.fillMaxWidth()) {
                    OutlinedTextField(
                        value = current,
                        onValueChange = { current = it },
                        label = { Text("Current password") },
                        visualTransformation = PasswordVisualTransformation(),
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    OutlinedTextField(
                        value = next,
                        onValueChange = { next = it },
                        label = { Text("New password") },
                        visualTransformation = PasswordVisualTransformation(),
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                    )
                    OutlinedTextField(
                        value = confirm,
                        onValueChange = { confirm = it },
                        label = { Text("New password again") },
                        visualTransformation = PasswordVisualTransformation(),
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                    )
                    error?.let {
                        Text(
                            it,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(top = 8.dp),
                        )
                    }
                }
            }
        },
        confirmButton = {
            if (done) TextButton(onClick = onDismiss) { Text("Done") }
            else TextButton(enabled = !busy, onClick = { submit() }) { Text("Change it") }
        },
        dismissButton = {
            if (!done) TextButton(onClick = onDismiss) { Text("Cancel") }
        },
    )
}
