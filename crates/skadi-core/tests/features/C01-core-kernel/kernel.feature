Feature: Core kernel — the shared vocabulary every crate speaks (C01)
  Typed UUID identifiers, the canonical AppError, external provider ids, the
  transport protocol, the persisted acquisition status, root-folder probing,
  media tiers and the NFO reader. Mirrors NzbDrone.Core's shared value types.

  @C01 @passing
  Scenario Outline: every domain identifier is a fresh, printable, JSON-transparent UUID
    Then the <id> identifier is a fresh, printable, JSON-transparent UUID

    Examples:
      | id             |
      | MovieId        |
      | MovieEditionId |
      | SeriesId       |
      | EpisodeId      |
      | SeasonId       |
      | BookId         |
      | BookFileId     |
      | AuthorId       |
      | ReleaseId      |
      | IndexerId      |
      | DownloaderId   |
      | ProfileId      |
      | QualityId      |
      | CustomFormatId |
      | NotifierId     |
      | RootFolderId   |

  @C01 @passing
  Scenario: identifiers of different entities are distinct types
    Then a movie id and a release id are distinct types even for the same UUID

  @C01 @passing
  Scenario Outline: the canonical error renders a descriptive message per variant
    When an <variant> error is raised with message "<message>"
    Then it displays as "<display>"

    Examples:
      | variant           | message         | display                                        |
      | NotFound          | movie 7         | Not found: movie 7                             |
      | Validation        | name required   | Validation error: name required                |
      | Config            | bad bind_addr   | Configuration error: bad bind_addr             |
      | Network           | HTTP 503        | Network error: HTTP 503                        |
      | Internal          | lock poisoned   | Internal error: lock poisoned                  |
      | InvalidTransition | ignored         | Invalid status transition: Missing -> Imported |

  @C01 @passing
  Scenario: I/O failures convert into the kernel error with the question-mark operator
    When a std I/O failure is propagated with the question-mark operator
    Then it is the Io variant
    And its message starts with "I/O error:"

  @C01 @passing
  Scenario: anyhow chains convert into the kernel error transparently
    When an anyhow error chain is propagated with the question-mark operator
    Then it is the Other variant
    And its message starts with "snatching release"

  @C01 @passing @SKADI-T-0525
  Scenario: a validation error identifies the offending field for the settings form
    When an Validation error is raised with message "api_key must be a non-empty string"
    Then the validation error names the offending field

  @C01 @passing
  Scenario: external ids omit absent providers and keep each provider's native shape
    When a library item knows only its TMDB id 603
    Then its external ids serialise as exactly:
      """
      {"tmdb":603}
      """
    And provider ids keep their upstream shape in JSON

  @C01 @passing
  Scenario Outline: the transport protocol gates indexers to downloaders
    Then the transport protocol <protocol> serialises as its name and back

    Examples:
      | protocol |
      | Torrent  |
      | Usenet   |

  @C01 @passing
  Scenario: a status persisted before the retry counter existed still loads
    Given a Failed status persisted before the attempts counter existed:
      """
      {"Failed":{"reason":"NoSuitableRelease","retry_at":null}}
      """
    Then it loads as Failed with 0 attempts and reason code "no_suitable_release"
    And a download failure reason keeps its transport detail

  @C01 @passing
  Scenario: an imported status records the root-relative file and its score
    When a file is imported at "Blade Runner (1982)/Blade Runner (1982) Bluray-1080p.mkv" with quality score 120
    Then the imported status round-trips with the relative path "Blade Runner (1982)/Blade Runner (1982) Bluray-1080p.mkv" and score 120

  @C01 @passing
  Scenario: a writable directory is a usable root folder and the probe leaves nothing behind
    When a writable directory is probed as a root folder
    Then the root is usable

  @C01 @passing
  Scenario: a file or a missing path is rejected as a root folder with a reason
    When a regular file is probed as a root folder
    Then the root is unusable because "path is not a directory"
    When a missing path is probed as a root folder
    Then the root is unusable because "path does not exist"

  @C01 @passing
  Scenario Outline: probed video dimensions map to the *arr resolution tiers
    Then a <width>x<height> video is tier "<tier>"

    Examples:
      | width | height | tier  |
      | 3840  | 2160   | 2160p |
      | 1920  | 1080   | 1080p |
      | 1920  | 800    | 1080p |
      | 1280  | 720    | 720p  |
      | 854   | 480    | 480p  |
      | 640   | 360    | SD    |

  @C01 @passing
  Scenario Outline: a Kodi sidecar yields provider ids and the title
    Then the NFO unique id of type "<kind>" in the sidecar is "<value>"

    Examples:
      | kind | value     |
      | tvdb | 280619    |
      | imdb | tt3230854 |
      | tmdb | none      |
