package com.skadi.core

/**
 * A normalized key for matching an author, series or title name across
 * sources: ASCII alphanumerics only (drops '.', spacing, punctuation and any
 * non-ASCII character), then lowercase. "A.G. Riddle" and "A. G. Riddle" key
 * equal, so a book's credit still finds the registered author's ASIN.
 *
 * The same rule is `skadi_audiobooks::discovery::name_key` on the server and
 * `author_key` in skadi-web; the three test one shared vector (SKADI-T-0648
 * F4). Filter first, lowercase second: "İ" (U+0130) is dropped, where
 * lowercasing first would leave an "i" behind and this copy would key
 * differently from the server.
 */
fun nameKey(s: String): String = s.filter { it.code < 128 && it.isLetterOrDigit() }.lowercase()
