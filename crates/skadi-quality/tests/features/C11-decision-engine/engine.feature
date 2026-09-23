Feature: Quality engine — definitions, profiles, custom formats, relevance
  Stable ranked quality definitions (REQ-DECIDE.4), profile verdicts over
  allowed/cutoff/upgrade (REQ-DECIDE.5–.7), custom-format rules with
  Preferred/Required/Ignored modes and an aggregate score (REQ-DECIDE.8/.9),
  and the title-relevance score the hunter gates on.

  @C11 @passing
  Scenario: the built-in definitions are ranked, stable and ship *arr-style profiles
    Given the built-in quality definitions
    And the standard profile
    Then the definitions are ranked with "HDTV-720p" below "WEBDL-1080p"
    And the definitions are ranked with "WEBDL-1080p" below "Bluray-1080p"
    And the definitions are ranked with "Bluray-1080p" below "Bluray-2160p"
    And definition ids are stable across calls
    And the built-in profiles include "Any, HD-1080p, Ultra-HD"
    And the standard profile allows 10 qualities with cutoff "Bluray-1080p"

  @C11 @passing
  Scenario: a parsed title classifies to the matching definition
    Given the release title "Heat.1995.1080p.BluRay.x264-GRP"
    When the title is classified
    Then it classifies as "Bluray-1080p"
    Given the release title "Heat.1995.720p.HDTV.x264-GRP"
    When the title is classified
    Then it classifies as "HDTV-720p"
    Given the release title "Heat.1995.2160p.WEB-DL.x265-GRP"
    When the title is classified
    Then it classifies as "WEBDL-2160p"

  @C11 @passing
  Scenario: a title with no resolution does not classify; resolution alone is enough
    # Resolution is the load-bearing token. Without one there is nothing to place
    # on the ladder, so the release stays unclassified and the caller records it
    # as Unknown rather than inventing a tier. With a resolution but no source,
    # SKADI-T-0432 reads the release as the HDTV tier (Sonarr's behaviour) —
    # anime and scene TV releases are routinely tagged "(1080p)" and nothing else,
    # and treating those as unclassifiable was the largest opaque quality reject.
    Given the release title "Heat.1995.x264-GRP"
    When the title is classified
    Then it does not classify to any quality
    Given the release title "Heat.1995.1080p.x264-GRP"
    When the title is classified
    Then it classifies as "HDTV-1080p"

  @C11 @passing
  Scenario Outline: profile verdicts over allowed, cutoff and upgrades
    Given a profile allowing "HDTV-720p, WEBDL-1080p, Bluray-1080p, Bluray-2160p" with cutoff "Bluray-1080p"
    And the library holds "<held>"
    When a "<candidate>" candidate is judged
    Then the verdict is <verdict>

    Examples:
      | held         | candidate     | verdict     |
      | HDTV-720p    | WEBDL-1080p   | Upgrade     |
      | WEBDL-1080p  | HDTV-720p     | Reject      |
      | WEBDL-1080p  | WEBDL-1080p   | Reject      |
      | Bluray-1080p | Bluray-2160p  | MeetsCutoff |
      | HDTV-720p    | Bluray-2160p  | Upgrade     |

  @C11 @passing
  Scenario: first acquisition accepts any allowed quality and rejects the rest
    Given a profile allowing "HDTV-720p, WEBDL-1080p" with cutoff "WEBDL-1080p"
    When a "HDTV-720p" candidate is judged
    Then the verdict is Accept
    When a "Bluray-1080p" candidate is judged
    Then the verdict is Reject

  @C11 @passing
  Scenario: a held file of unknown quality is still upgradable
    Given a profile allowing "HDTV-720p, WEBDL-1080p" with cutoff "WEBDL-1080p"
    And the library holds a file of unknown quality
    When a "HDTV-720p" candidate is judged
    Then the verdict is Upgrade

  @C11 @passing
  Scenario: upgrades disabled means a held file is never replaced
    Given a profile allowing "HDTV-720p, WEBDL-1080p" with cutoff "WEBDL-1080p"
    And upgrades are disabled
    And the library holds "HDTV-720p"
    When a "WEBDL-1080p" candidate is judged
    Then the verdict is Reject

  @C11 @passing
  Scenario: the format-score floor is inclusive
    Given the standard profile
    Then the profile's score floor of 10 accepts 10 and rejects 9

  @C11 @passing
  Scenario: custom formats sum every matching format, including penalties
    Given the release title "Heat.1995.1080p.BluRay.x265-GRP"
    And a custom format "HEVC" with codec "x265" scoring 10
    And a custom format "BluRay" with source "BluRay" scoring 5
    And a custom format "Bad group" with title regex "-GRP$" scoring -20
    And a custom format "2160p" with resolution "2160p" scoring 100
    When the release is scored
    Then the aggregate format score is -5
    And the matched formats are "HEVC, BluRay, Bad group"

  @C11 @passing
  Scenario: a format needs every rule to match, and a rule-less format never matches
    Given the release title "Heat.1995.1080p.BluRay.x264-GRP"
    And a custom format "BluRay x265" with source "BluRay" and codec "x265" scoring 50
    And a custom format "Empty" with no rules scoring 50
    When the release is scored
    Then the aggregate format score is 0

  @C11 @passing
  Scenario: size and indexer-flag rules read release metadata
    Given the release title "Heat.1995.1080p.BluRay.x264-GRP"
    And a custom format "Small" with a maximum size of 8 GB scoring 3
    And a custom format "Freeleech" with indexer flag "freeleech" scoring 7
    When the release of 4 GB with flags "FREELEECH" is scored
    Then the aggregate format score is 10
    When the release of 20 GB with flags "" is scored
    Then the aggregate format score is 0

  @C11 @passing
  Scenario: edition and language rules read the parsed fields
    Given the release title "Kingdom.of.Heaven.2005.Directors.Cut.FRENCH.1080p.BluRay.x264-GRP"
    And a custom format "DC" with edition "director" scoring 4
    And a custom format "FR" with language "french" scoring 2
    When the release is scored
    Then the aggregate format score is 6

  @C11 @passing
  Scenario: required and ignored formats are reported for the hard gates
    Given the release title "Heat.1995.1080p.BluRay.x264-EVO"
    And a required custom format "HEVC" with title regex "x265"
    And an ignored custom format "EVO" with title regex "-EVO$"
    When the release is scored
    Then the required format "HEVC" is reported missing
    And the ignored format "EVO" is reported present
    And the aggregate format score is 0

  @C11 @passing
  Scenario: a malformed user regex is refused at write time and never matches
    Given the release title "Heat.1995.1080p.BluRay.x264-GRP"
    And a custom format "Broken" with title regex "(" scoring 99
    When the release is scored
    Then the aggregate format score is 0
    And the title regex "(" is rejected at validation

  @C11 @passing
  Scenario: title relevance measures coverage of the wanted title and match tightness
    When "The.Matrix.1999.1080p.BluRay.x264-GRP" is scored against the wanted title "The Matrix"
    Then the title coverage is 1.0
    When "Blade.Runner.2049.1080p.BluRay.x264-GRP" is scored against the wanted title "The Matrix"
    Then the title coverage is 0.0
    When "Storm.Front.Weather.Documentary.2019.1080p" is scored against the wanted title "Storm Front"
    Then the title coverage is 1.0
    And the precision is below 0.5

  @C11 @passing
  Scenario: the author alias tells a real audiobook from coincidental junk
    When "Jim Butcher - Storm Front (The Dresden Files #1) MP3 64kbps" is scored against the wanted title "Storm Front" by "Jim Butcher"
    Then the author is present in the release
    When "Storm Front - Evanescence Live [MP3 320kbps]" is scored against the wanted title "Storm Front" by "Jim Butcher"
    Then the author is absent in the release
