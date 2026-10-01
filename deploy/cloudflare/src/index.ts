import { Container, getContainer } from "@cloudflare/containers";

interface Env {
  STEIN_FOLLOWER: DurableObjectNamespace<SteinFollower>;
  /** The validator's public URL (a Cloudflare Tunnel to the local Docker node). */
  STEIN_LEADER: string;
  /** The only genesis this follower accepts; obtained from the validator operator. */
  STEIN_GENESIS_HASH: string;
}

/** One Stein follower node: forwards transactions to the leader, and downloads
 * and fully re-verifies the leader's blocks. */
export class SteinFollower extends Container<Env> {
  defaultPort = 8645;
  // Keep following for a while after the last request. When it sleeps, the
  // next request starts it again and it re-syncs from genesis.
  sleepAfter = "1h";

  constructor(ctx: DurableObjectState<{}>, env: Env) {
    super(ctx, env, {
      envVars: {
        STEIN_ROLE: "follower",
        STEIN_LEADER: env.STEIN_LEADER,
        STEIN_GENESIS_HASH: env.STEIN_GENESIS_HASH,
        STEIN_RPC: "0.0.0.0:8645",
      },
    });
  }
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    if (!env.STEIN_LEADER || !/^[0-9a-f]{64}$/.test(env.STEIN_GENESIS_HASH)) {
      return new Response("follower not configured: deploy with deploy.sh\n", { status: 503 });
    }
    // A single follower instance; every request goes to it.
    return getContainer(env.STEIN_FOLLOWER, "follower").fetch(request);
  },
};
