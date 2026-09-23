Feature: Filesystem-safe path components (C21)
  Every rendered component is sanitized: `/\:*?"<>|` become `_`, whitespace becomes
  the configured replacement, runs collapse, ends are trimmed. Mirrors Sonarr/Radarr
  "Replace Illegal Characters".

  @C21 @passing
  Scenario: illegal characters are replaced and whitespace collapses
    When "What: If?/Maybe" is sanitized
    Then the result is "What_If_Maybe"
    When "The Matrix  (1999)" is sanitized
    Then the result is "The_Matrix_(1999)"
    When "  trim  " is sanitized
    Then the result is "trim"

  @C21 @passing
  Scenario: the space-preserving mode keeps spaces but still replaces illegal characters
    Given the space replacement is " "
    When "A: B" is sanitized
    Then the result is "A_ B"
    When "A   B" is sanitized
    Then the result is "A B"

  @C21 @passing
  Scenario: curly quotes and other unicode survive sanitization
    When "‘Salem’s Lot" is sanitized
    Then the result is "‘Salem’s_Lot"

  @C21 @passing
  Scenario: kebab-casing folds punctuation and case into dashes
    When "Rebel Moon - Part One: A Child of Fire" is kebab-cased
    Then the result is "rebel-moon-part-one-a-child-of-fire"
    When "‘Salem’s Lot" is kebab-cased
    Then the result is "salem-s-lot"
    When "Big Buck Bunny" is kebab-cased
    Then the result is "big-buck-bunny"

  @C21 @passing
  Scenario: a path-like title cannot inject separators
    When "../../etc/passwd" is sanitized
    Then the result is ".._.._etc_passwd"

  @C21 @passing @SKADI-T-0415
  Scenario: a title consisting only of dots cannot escape the library root
    Given the token "Title" is ".."
    When folder template "{Title}" and file template "{Title}" build a path under "/library/movie" with extension ".mkv"
    Then the path stays under the root

  @C21 @passing @SKADI-T-0421
  Scenario: a name ending in a dot or space is trimmed for Windows/SMB shares
    When "Mr. Robot S01E01." is sanitized
    Then the result has no trailing dot or space

  @C21 @passing @SKADI-T-0421
  Scenario: a Windows reserved device name is escaped
    When "CON" is sanitized
    Then the result is not a Windows reserved device name

  @C21 @passing @SKADI-T-0421
  Scenario: an overlong component is truncated to the filesystem name limit
    When "Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" is sanitized
    Then the result is at most 255 bytes

  @C21 @passing @SKADI-T-0421
  Scenario: decomposed unicode is normalised so one title yields one on-disk name
    When "Amélie" is sanitized
    Then the result is in Unicode NFC form
