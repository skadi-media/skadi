Feature: Domain module framework — the plug-in contract every media type implements (C23)
  The trait set (LibraryItem / Acquirable / DomainModule / Worker) and the shared
  AcquisitionStatus lifecycle that movies, television and audiobooks all build on.

  @C23 @passing
  Scenario: a domain item exposes its identity and iterates its acquirables
    Given a domain item "Blade Runner" with acquirables in statuses "Missing, Cutoff, Failed, Imported"
    Then the item exposes title "Blade Runner", kind Movie and 4 acquirables
    And exactly 2 of its acquirables are wanted

  @C23 @passing
  Scenario Outline: the wanted -> imported lifecycle only moves forward along legal edges
    When an acquirable moves from <from> to <to>
    Then the transition is legal

    Examples:
      | from        | to          |
      | Missing     | Searching   |
      | Searching   | Snatched    |
      | Searching   | Failed      |
      | Searching   | Missing     |
      | Snatched    | Downloading |
      | Snatched    | Failed      |
      | Downloading | Imported    |
      | Downloading | Failed      |
      | Imported    | Cutoff      |
      | Imported    | Searching   |
      | Cutoff      | Searching   |
      | Failed      | Searching   |
      | Failed      | Missing     |
      | Downloading | Downloading |

  @C23 @passing
  Scenario Outline: skipping stages of the lifecycle is rejected
    When an acquirable moves from <from> to <to>
    Then the transition is rejected

    Examples:
      | from     | to          |
      | Missing  | Imported    |
      | Missing  | Downloading |
      | Cutoff   | Imported    |
      | Imported | Snatched    |
      | Failed   | Imported    |

  @C23 @passing
  Scenario: failure reasons carry stable machine codes and statuses round-trip through JSON
    Then the failure reason NoSuitableRelease has code "no_suitable_release"
    And the failure reason DownloadFailed has code "download_failed"
    And the failure reason ImportFailed has code "import_failed"
    And the status Imported survives a JSON round-trip
    And the status Failed survives a JSON round-trip

  @C23 @passing
  Scenario Outline: each media kind maps to one media type and one library subfolder
    Then media kind <kind> belongs to type <type> under library subfolder "<subfolder>"

    Examples:
      | kind      | type  | subfolder  |
      | Movie     | video | movie      |
      | Series    | video | television |
      | Audiobook | audio | audiobook  |
      | Music     | audio | music      |
      | Book      | print | book       |

  @C23 @passing
  Scenario: a compiled-in module exposes identity, dual-backend migrations and cancellable workers
    Given a domain module compiled in as a plug-in
    Then the module reports name "dummy" and kind Book
    And the module ships one migration set per backend
    When the supervisor spawns the module's workers and then cancels them
    Then every worker ran and stopped promptly on cancellation

  @C23 @gap
  Scenario: a module can describe itself for a domains management screen (display name, version, health)
    Given a domain module compiled in as a plug-in
    Then the module exposes a display name and lifecycle health for a domains screen
