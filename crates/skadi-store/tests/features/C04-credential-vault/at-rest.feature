Feature: Credential vault — secrets at rest (C04)
  Provider secrets live in the `credentials` table keyed by (owner kind, owner
  id). With SKADI_SECRET_KEY set they are sealed with ChaCha20-Poly1305 under a
  key derived from it (a random 12-byte nonce is stored alongside); without a
  key they are stored as plaintext with a startup warning. Callers always see
  plain strings. Sonarr/Radarr always encrypt (per-install key in config.xml).

  @C04 @passing @serial
  Scenario: without a master key the secret is stored as plaintext and marked so
    Given no master key is configured
    And a migrated sqlite store
    When the indexers "ix-1" secret "my-api-key" is stored
    Then the indexers "ix-1" row stores the plaintext bytes of "my-api-key" with no nonce
    When the indexers "ix-1" secret is read
    Then the secret reads back as "my-api-key"

  @C04 @passing @serial
  Scenario: with a master key the secret is sealed at rest and transparent to callers
    Given the master key "correct horse battery staple" is configured
    And a migrated sqlite store
    When the indexers "ix-1" secret "my-api-key" is stored
    Then the indexers "ix-1" row stores ciphertext that is not "my-api-key" with a 12-byte nonce
    When the indexers "ix-1" secret is read
    Then the secret reads back as "my-api-key"

  @C04 @passing @serial
  Scenario: the same secret sealed twice never produces the same bytes
    Given the master key "k" is configured
    And a migrated sqlite store
    When the downloaders "dl-a" secret "hunter2" is stored
    And the downloaders "dl-b" secret "hunter2" is stored
    Then the two encryptions of "hunter2" for downloaders "dl-a" and "dl-b" differ

  @C04 @passing @serial
  Scenario: a secret is replaced in place and removed on delete
    Given the master key "k" is configured
    And a migrated sqlite store
    When the notifiers "n1" secret "first" is stored
    And the notifiers "n1" secret "rotated" is stored
    And the notifiers "n1" secret is read
    Then the secret reads back as "rotated"
    When the notifiers "n1" secret is deleted
    And the notifiers "n1" secret is read
    Then the secret is absent

  @C04 @passing @serial
  Scenario: a plaintext row written before a key existed still reads under the key
    Given no master key is configured
    And a migrated sqlite store
    And the indexers "legacy" secret "old-key" is stored
    When the daemon restarts with the master key "new-key"
    And the restarted daemon reads the indexers "legacy" secret
    Then the secret reads back as "old-key"

  @C04 @passing @serial
  Scenario: an encrypted row cannot be read once the key is removed
    Given the master key "k" is configured
    And a migrated sqlite store
    And the indexers "sealed" secret "s3cret" is stored
    When the daemon restarts with no master key
    And the restarted daemon reads the indexers "sealed" secret
    Then the read fails mentioning "SKADI_SECRET_KEY is not set"

  @C04 @passing @serial
  Scenario: an encrypted row cannot be read under a different key
    Given the master key "key-a" is configured
    And a migrated sqlite store
    And the indexers "sealed" secret "s3cret" is stored
    When the daemon restarts with the master key "key-b"
    And the restarted daemon reads the indexers "sealed" secret
    Then the read fails mentioning "decryption failed"

  @C04 @gap @serial
  Scenario: plaintext rows are sealed once a master key is configured
    Given no master key is configured
    And a migrated sqlite store
    And the indexers "legacy" secret "old-key" is stored
    When the daemon restarts with the master key "new-key"
    And the restarted daemon reads the indexers "legacy" secret
    Then the indexers "legacy" row has been sealed now that a key exists

  @C04 @gap @serial
  Scenario: the master key can be rotated without losing every stored credential
    Given the master key "key-a" is configured
    And a migrated sqlite store
    And the indexers "sealed" secret "s3cret" is stored
    When the daemon restarts with the master key "key-b"
    And the restarted daemon reads the indexers "sealed" secret
    Then stored credentials are re-encrypted under the new master key
