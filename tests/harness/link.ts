// The connection of one server to one service, through a relay in the
// test process, so that a test can cut it and restore it.

import { createConnection, createServer, type Server as NetServer, type Socket } from "node:net"

export class Link {
  private open = new Set<Socket>()
  private isCut = false

  private constructor(
    private relay: NetServer,
    /** `127.0.0.1:<port>`: where the server finds the service. */
    readonly address: string
  ) {}

  /** A relay to `target` (`host:port`). */
  static start(target: string): Promise<Link> {
    const [host, port] = target.split(":")
    return new Promise((resolve, reject) => {
      const relay = createServer()
      relay.once("error", reject)
      relay.listen(0, "127.0.0.1", () => {
        const link = new Link(relay, `127.0.0.1:${(relay.address() as { port: number }).port}`)
        relay.on("connection", (from) => link.accept(from, host, Number(port)))
        resolve(link)
      })
    })
  }

  private accept(from: Socket, host: string, port: number) {
    if (this.isCut) return from.destroy()
    const to = createConnection({ host, port })
    for (const [a, b] of [
      [from, to],
      [to, from],
    ]) {
      this.open.add(a)
      a.on("error", () => {})
      a.on("close", () => {
        this.open.delete(a)
        b.destroy()
      })
      a.pipe(b)
    }
  }

  /** Breaks every connection and refuses new ones, as an outage does. */
  cut() {
    this.isCut = true
    for (const s of this.open) s.destroy()
  }

  restore() {
    this.isCut = false
  }

  close() {
    this.cut()
    this.relay.close()
  }
}
