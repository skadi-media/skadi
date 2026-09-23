Feature: Sonarr/Radarr naming features the engine does not have yet (C21)
  Each scenario asserts an *arr naming behaviour and fails honestly while it is
  missing: format modifiers, save-time validation, quality/release-group tokens,
  and the colon-replacement style.

  @C21 @passing @SKADI-T-0420
  Scenario: numeric padding modifiers like Sonarr's {season:00}
    Given the token "Season" is "5"
    Then the format modifier "Season {Season:00}" pads to "Season 05"

  @C21 @passing @SKADI-T-0420
  Scenario: an unknown token is reported when the format is saved
    Given the movie token set the domain supplies
    Then the template "{Title} ({Yaer})" is reported invalid mentioning "Yaer"

  @C21 @passing @SKADI-T-0420
  Scenario: an unbalanced brace is reported when the format is saved
    Given the movie token set the domain supplies
    Then the template "{Title" is reported invalid mentioning "brace"

  @C21 @passing @SKADI-T-0420
  Scenario: the movie file format can include the quality like Radarr's {Quality Full}
    Given the movie token set the domain supplies
    When the component template "{Title} ({Year}) {Quality Full}" is rendered
    Then the rendered name contains the quality "Bluray-1080p"

  @C21 @passing @SKADI-T-0420
  Scenario: the file format can include the release group like {Release Group}
    Given the movie token set the domain supplies
    When the component template "{Title} ({Year})-{Release Group}" is rendered
    Then the rendered name contains the release group "GRP"

  @C21 @passing @SKADI-T-0420
  Scenario: the operator chooses how a colon is replaced (Sonarr "Colon Replacement")
    Given the space replacement is " "
    When "Rebel Moon: Part One" is sanitized
    Then the colon renders as "Rebel Moon - Part One" under the "Space Dash" colon style
