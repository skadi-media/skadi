Feature: Downloader settings rows
  crates/skadi-downloaders/src/config.rs — the kind-tagged `DownloaderConfig`
  stored in a `downloaders` settings row, and what the provider factory makes of
  it. Since SKADI-T-0517 there is exactly one kind: the built-in DB-queue
  downloader, whose transfers run in the VPN-isolated worker. Sonarr's equivalent
  page offers a list of clients (qBittorrent, SABnzbd, NZBGet) with per-client
  settings; Skadi's offers one, so the interesting behaviour is what it defaults
  and what it refuses.

  @C16 @passing
  Scenario: the built-in skadi kind defaults its dirs and is flagged built-in
    Given the downloader settings row:
      """
      { "kind": "skadi", "name": "built-in" }
      """
    When the provider factory builds the downloader
    Then the settings row is the built-in downloader
    And the settings row resolves skadi dirs "/data/downloads/incomplete" and "/data/downloads/complete"

  @C16 @passing @SKADI-T-0517
  Scenario: a downloader row left over from qBittorrent is refused, not obeyed
    Given the downloader settings row:
      """
      { "kind": "qbittorrent", "name": "qb", "base_url": "http://qbit:8080", "username": "admin" }
      """
    When the provider factory builds the downloader
    Then the settings row is not a valid downloader kind

  @C16 @gap
  Scenario: a Usenet client kind (sabnzbd) is a valid settings row
    Given the downloader settings row:
      """
      { "kind": "sabnzbd", "name": "sab", "base_url": "http://sab:8080", "category": "movies" }
      """
    Then the settings row is a valid downloader kind

  @C16 @gap
  Scenario: the built-in downloader row carries Sonarr's client settings (remove completed, remote path mappings, priority)
    Given the downloader settings row:
      """
      { "kind": "skadi", "name": "built-in" }
      """
    Then the stored settings row carries the field "remove_completed_downloads"
    And the stored settings row carries the field "remote_path_mappings"
