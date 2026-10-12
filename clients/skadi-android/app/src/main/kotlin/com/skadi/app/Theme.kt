package com.skadi.app

import android.app.Activity
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat

/**
 * The app theme. [System] follows the phone's dark mode.
 *
 * [key] is what [Settings] stores (SKADI-T-0721): stable, so renaming an
 * entry does not lose anyone's choice.
 */
enum class SkadiThemeMode(val key: String, val label: String) {
    System("system", "System"),
    Light("light", "Light"),
    Dark("dark", "Dark"),
    Hyper("hyper", "Hyper"),
    ;

    companion object {
        /** A stored key back to a mode. Nothing stored, or an unknown key, is [System]. */
        fun fromKey(key: String?): SkadiThemeMode = entries.firstOrNull { it.key == key } ?: System
    }
}

/**
 * Aurora colour tokens, by their Aurora names.
 *
 * Source: colliery-io-aurora 0.5.0, `style/tokens.css` (light and dark: each
 * token is `light-dark(<light>, <dark>)`) and `style/hyper.css`
 * (`:root[data-theme="hyper"]`). To move to a later Aurora, compare those two
 * files with the values below. This file is the only place in the app that
 * holds a raw colour.
 */
private class Aurora(
    val bg: Color,
    val panel: Color,
    val panel2: Color,
    val field: Color,
    val control: Color,
    val controlHover: Color,
    val border: Color,
    val borderControl: Color,
    val fg: Color,
    val muted: Color,
    val ice: Color,
    val iceFg: Color,
    val gold: Color,
    val goldFg: Color,
    val ok: Color,
    val okFg: Color,
    val bad: Color,
    val badFg: Color,
    val onStatus: Color,
    val onAccent: Color,
    val tooltipBg: Color,
    val tooltipFg: Color,
    val scrim: Color,
)

private val AuroraLight = Aurora(
    bg = Color(0xFFF3F5F8),
    panel = Color(0xFFFFFFFF),
    panel2 = Color(0xFFF7F8FA),
    field = Color(0xFFFFFFFF),
    control = Color(0xFFFFFFFF),
    controlHover = Color(0xFFEAEEF4),
    border = Color(0xFFDCE1E8),
    borderControl = Color(0xFF7C8695),
    fg = Color(0xFF18202B),
    muted = Color(0xFF525D6E),
    ice = Color(0xFF1F60CC),
    iceFg = Color(0xFF174CA6),
    gold = Color(0xFF8A5800),
    goldFg = Color(0xFF7A4D00),
    ok = Color(0xFF15793B),
    okFg = Color(0xFF126A33),
    bad = Color(0xFFC22A2A),
    badFg = Color(0xFFA51F1F),
    onStatus = Color(0xFFFFFFFF),
    onAccent = Color(0xFFFFFFFF),
    tooltipBg = Color(0xFF1B2330),
    tooltipFg = Color(0xFFF1F4F8),
    scrim = Color(0x52101826), // rgba(16, 24, 38, 0.32)
)

private val AuroraDark = Aurora(
    bg = Color(0xFF0E1116),
    panel = Color(0xFF161A21),
    panel2 = Color(0xFF13171E),
    field = Color(0xFF0A0C10),
    control = Color(0xFF1B2129),
    controlHover = Color(0xFF20262F),
    border = Color(0xFF232A34),
    borderControl = Color(0xFF5D6776),
    fg = Color(0xFFE6E9EE),
    muted = Color(0xFF8B95A3),
    ice = Color(0xFF7FB2FF),
    iceFg = Color(0xFF7FB2FF),
    gold = Color(0xFFD8A657),
    goldFg = Color(0xFFD8A657),
    ok = Color(0xFF4BD07F),
    okFg = Color(0xFF4BD07F),
    bad = Color(0xFFF06464),
    badFg = Color(0xFFF06464),
    onStatus = Color(0xFF0B0D10),
    onAccent = Color(0xFF0B0D10),
    tooltipBg = Color(0xFF05070A),
    tooltipFg = Color(0xFFE6E9EE),
    scrim = Color(0xA605070A), // rgba(5, 7, 10, 0.65)
)

private val AuroraHyper = Aurora(
    bg = Color(0xFF07070D),
    panel = Color(0xFF0F0F1C),
    panel2 = Color(0xFF0C0C17),
    field = Color(0xFF050509),
    control = Color(0xFF161628),
    controlHover = Color(0xFF1D1D34),
    border = Color(0xFF262642),
    borderControl = Color(0xFF6C69A6),
    fg = Color(0xFFECEBFF),
    muted = Color(0xFF9D9ACB),
    ice = Color(0xFF00E5FF),
    iceFg = Color(0xFF00E5FF),
    gold = Color(0xFFFFC400),
    goldFg = Color(0xFFFFC400),
    ok = Color(0xFF3DFF7A),
    okFg = Color(0xFF3DFF7A),
    bad = Color(0xFFFF3D6E),
    badFg = Color(0xFFFF3D6E),
    onStatus = Color(0xFF05050A),
    onAccent = Color(0xFF05050A),
    tooltipBg = Color(0xFF03030A),
    tooltipFg = Color(0xFFECEBFF),
    scrim = Color(0xB803030A), // rgba(3, 3, 10, 0.72)
)

/** `color-mix(in srgb, hue p%, base)`: how Aurora builds its `--x-bg` tints. */
private fun mix(hue: Color, p: Float, base: Color) = Color(
    red = base.red + (hue.red - base.red) * p,
    green = base.green + (hue.green - base.green) * p,
    blue = base.blue + (hue.blue - base.blue) * p,
)

/** Aurora's `--x-bg` tint (11 % of the hue), flattened onto a panel. */
private fun Aurora.tint(hue: Color) = mix(hue, 0.11f, panel)

/**
 * Aurora roles on the Material slots the screens read.
 *
 * `primary` is for *actions* (play, the selected tab, the active rail
 * letter). Titles are `onBackground`: a heading in the accent colour
 * competes with the buttons next to it, which is part of what made the
 * old headers feel busy (SKADI-T-0577).
 */
private fun Aurora.scheme(dark: Boolean): ColorScheme {
    val base = if (dark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = ice, // actions
        onPrimary = onAccent,
        primaryContainer = tint(ice),
        onPrimaryContainer = iceFg,
        // On inverseSurface, which is dark in every theme: light takes the dark ice.
        inversePrimary = if (dark) ice else AuroraDark.ice,
        secondary = ok, // "up next" / success
        onSecondary = onStatus,
        secondaryContainer = tint(ice), // selected tab / chip pill
        onSecondaryContainer = iceFg,
        tertiary = gold, // offline / warning / wanted
        onTertiary = onStatus,
        tertiaryContainer = tint(gold),
        onTertiaryContainer = goldFg,
        error = bad, // destructive (unpair)
        onError = onStatus,
        errorContainer = tint(bad),
        onErrorContainer = badFg,
        background = bg,
        onBackground = fg,
        surface = panel2,
        onSurface = fg,
        surfaceVariant = controlHover, // chips, placeholders, bars
        onSurfaceVariant = muted, // muted / secondary text
        surfaceTint = ice,
        surfaceBright = panel,
        surfaceDim = bg,
        surfaceContainerLowest = field,
        surfaceContainerLow = panel,
        surfaceContainer = panel, // nav bar, mini-player, sheets
        surfaceContainerHigh = control, // dialogs
        surfaceContainerHighest = controlHover,
        inverseSurface = tooltipBg, // snackbars
        inverseOnSurface = tooltipFg,
        outline = borderControl,
        outlineVariant = border,
        scrim = scrim,
    )
}

@Composable
fun SkadiTheme(mode: SkadiThemeMode = SkadiThemeMode.System, content: @Composable () -> Unit) {
    val systemDark = isSystemInDarkTheme()
    val (aurora, dark) = when (mode) {
        SkadiThemeMode.System -> if (systemDark) AuroraDark to true else AuroraLight to false
        SkadiThemeMode.Light -> AuroraLight to false
        SkadiThemeMode.Dark -> AuroraDark to true
        SkadiThemeMode.Hyper -> AuroraHyper to true
    }
    val scheme = aurora.scheme(dark)
    // Status and navigation bars: the page colour, with dark icons on light.
    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as? Activity)?.window ?: return@SideEffect
            @Suppress("DEPRECATION")
            window.statusBarColor = scheme.background.toArgb()
            @Suppress("DEPRECATION")
            window.navigationBarColor = scheme.surfaceContainer.toArgb()
            WindowCompat.getInsetsController(window, view).apply {
                isAppearanceLightStatusBars = !dark
                isAppearanceLightNavigationBars = !dark
            }
        }
    }
    MaterialTheme(colorScheme = scheme, content = content)
}
