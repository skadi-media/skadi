package com.skadi.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import android.os.Build
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.skadi.core.LoginResult
import com.skadi.core.ProbeResult
import com.skadi.core.Pairing
import com.skadi.pairing.NsdDiscovery
import kotlinx.coroutines.launch
import androidx.compose.runtime.rememberCoroutineScope

/**
 * Pairing (SKADI-T-0341): NSD-discover `_skadi._tcp`, fetch the injected
 * token from `/`, persist. Falls back to a manual host:port field when
 * discovery times out (emulators, odd networks). Zero typing on the happy
 * path — the squire experience.
 */
@Composable
fun PairingScreen(onPaired: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    // searching | login | connecting
    var phase by remember { mutableStateOf("searching") }
    var manualHost by remember { mutableStateOf("") }
    /** The server we found or were told about, once it has answered. */
    var serverUrl by remember { mutableStateOf<String?>(null) }
    var username by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }

    /** Save and hand off — `Root` is watching the store and redraws. */
    suspend fun finish(baseUrl: String, token: String) {
        Settings.save(context, baseUrl, token)
        onPaired()
    }

    /**
     * Ask a candidate address what it is. An open-mode daemon needs no
     * credential and we are done; a secured one puts us on the login form.
     */
    suspend fun probe(baseUrl: String, quietIfMissing: Boolean = false) {
        phase = "connecting"
        when (Pairing.probe(baseUrl)) {
            ProbeResult.OpenMode -> finish(baseUrl, "")
            ProbeResult.NeedsLogin -> {
                serverUrl = baseUrl
                error = null
                phase = "login"
            }
            ProbeResult.NotSkadi -> {
                error = "Something answered at $baseUrl but it isn't Skadi."
                phase = "login"
            }
            ProbeResult.Unreachable -> {
                if (!quietIfMissing) error = "Couldn't reach $baseUrl."
                phase = "login"
            }
        }
    }

    suspend fun signIn() {
        val base = serverUrl ?: "http://${manualHost.trim()}"
        phase = "connecting"
        when (val r = Pairing.login(base, username.trim(), password, Build.MODEL ?: "Android")) {
            is LoginResult.Ok -> finish(base, r.token)
            LoginResult.Rejected -> {
                error = "That name and password don't match."
                password = ""
                phase = "login"
            }
            LoginResult.Unreachable -> {
                error = "Couldn't reach $base."
                phase = "login"
            }
        }
    }

    // QR scanner (zxing): parse a skadi://pair?host=&token= URI from the web
    // UI's "Set up the Android app" card → paired in one scan (SKADI-T-0350).
    val scanLauncher = androidx.activity.compose.rememberLauncherForActivityResult(
        com.journeyapps.barcodescanner.ScanContract(),
    ) { result ->
        val contents = result.contents
        if (contents != null) {
            val uri = runCatching { android.net.Uri.parse(contents) }.getOrNull()
            val host = uri?.getQueryParameter("host")
            val token = uri?.getQueryParameter("token")
            if (uri?.scheme == "skadi" && !host.isNullOrBlank()) {
                scope.launch {
                    // A link that still carries a token keeps working, so
                    // nothing issued before logins existed breaks; one that
                    // only names a host just points us at the login form
                    // (SKADI-T-0622).
                    if (!token.isNullOrBlank()) finish("http://$host", token)
                    else probe("http://$host")
                }
            } else {
                error = "That QR isn't a skadi pairing code."
                phase = "manual"
            }
        }
    }
    fun launchScan() {
        val opts = com.journeyapps.barcodescanner.ScanOptions()
            .setDesiredBarcodeFormats(com.journeyapps.barcodescanner.ScanOptions.QR_CODE)
            .setPrompt("Scan the pairing QR from Skadi → Listen → Android app")
            .setBeepEnabled(false)
            .setOrientationLocked(false)
        // Decode inverted codes too (light modules on a dark screen), so a QR
        // shown on a dark web page or another phone still reads (SKADI-T-0616).
        opts.addExtra(com.google.zxing.client.android.Intents.Scan.SCAN_TYPE, com.google.zxing.client.android.Intents.Scan.MIXED_SCAN)
        scanLauncher.launch(opts)
    }

    LaunchedEffect(Unit) {
        val found = NsdDiscovery(context).discover(timeoutMs = 5000)
        if (found != null) {
            probe("http://${found.first}:${found.second}", quietIfMissing = true)
        } else {
            phase = "login"
        }
    }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("✳ Skadi", style = MaterialTheme.typography.headlineLarge, color = MaterialTheme.colorScheme.primary)
        when (phase) {
            "searching", "connecting" -> {
                CircularProgressIndicator(modifier = Modifier.padding(24.dp))
                Text(
                    if (phase == "searching") "Looking for Skadi on your Wi-Fi…" else "Signing in…",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (phase == "searching") {
                    Text(
                        "Your phone and the Skadi server must be on the same network.",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
            else -> {
                error?.let {
                    Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(8.dp))
                }
                // The server is only asked for once. After it has answered, the
                // screen is a plain sign-in and the address stays out of the way.
                if (serverUrl == null) {
                    Text(
                        "Where is Skadi?",
                        modifier = Modifier.padding(top = 16.dp, bottom = 4.dp),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OutlinedTextField(
                        value = manualHost,
                        onValueChange = { manualHost = it },
                        label = { Text("host:port (e.g. skadi.tailnet.ts.net:8080)") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = true,
                    )
                    Button(
                        onClick = { scope.launch { probe("http://${manualHost.trim()}") } },
                        enabled = manualHost.isNotBlank(),
                        modifier = Modifier.padding(top = 12.dp),
                    ) { Text("Continue") }
                    TextButton(onClick = { launchScan() }) { Text("Scan a QR instead") }
                    TextButton(onClick = {
                        phase = "searching"
                        error = null
                        scope.launch {
                            val found = NsdDiscovery(context).discover(timeoutMs = 5000)
                            if (found != null) probe("http://${found.first}:${found.second}")
                            else phase = "login"
                        }
                    }) { Text("Search again") }
                } else {
                    Text(
                        "Sign in",
                        modifier = Modifier.padding(top = 16.dp),
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        serverUrl.orEmpty().removePrefix("http://"),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(bottom = 12.dp),
                    )
                    OutlinedTextField(
                        value = username,
                        onValueChange = { username = it },
                        label = { Text("Name") },
                        modifier = Modifier.fillMaxWidth(),
                        singleLine = true,
                    )
                    OutlinedTextField(
                        value = password,
                        onValueChange = { password = it },
                        label = { Text("Password") },
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = KeyboardOptions(
                            keyboardType = KeyboardType.Password,
                            imeAction = ImeAction.Go,
                        ),
                        keyboardActions = KeyboardActions(onGo = { scope.launch { signIn() } }),
                        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                        singleLine = true,
                    )
                    Button(
                        onClick = { scope.launch { signIn() } },
                        enabled = username.isNotBlank() && password.isNotEmpty(),
                        modifier = Modifier.padding(top = 12.dp),
                    ) { Text("Sign in") }
                    TextButton(onClick = { serverUrl = null; error = null }) {
                        Text("Use a different server")
                    }
                }
            }
        }
    }
}
