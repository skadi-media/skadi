-- The egress IP the download worker observes (SKADI-T-0683). The worker asks
-- gluetun's control server on its own loopback (it shares gluetun's network
-- namespace) for the public IP, and writes the answer on every heartbeat. The
-- daemon's `vpn` health check compares it with the exit IP gluetun reports to
-- the daemon: a worker outside the namespace cannot reach gluetun on loopback,
-- so it reports NULL, and a worker behind a different tunnel reports another IP.
-- NULL also means an older worker, or a deploy without gluetun.
ALTER TABLE worker_status ADD COLUMN egress_ip TEXT;
