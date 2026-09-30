# Network

![The Network screen](screenshots/network.png)

The network interfaces from `/proc/net/dev` and `/sys/class/net`.

- **Rates:** live receive and transmit rates, computed from the time actually
  elapsed between samples, and the total counters.
- **Addresses:** IPv4 and IPv6.
- **State:** `● up`, `○ down`, `◌ dormant`, or `◌ unknown` (for example the
  loopback interface).
- **Details:** `Enter` shows the MAC address, MTU, packet counts, errors and
  dropped packets.

![Interface details](screenshots/network_detail.png)

Rates start over cleanly when an interface appears, disappears, or
`/proc/net/dev` cannot be read for a moment, instead of showing a spike.
