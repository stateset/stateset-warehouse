import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AuthKitProvider, getClaims, useAuth } from "@workos-inc/authkit-react";
import App from "./App";

export type WarehouseAuth = {
  getAccessToken: () => Promise<string>;
  signOut: () => void;
};
type Config =
  | { provider: "api_key" }
  | { provider: "workos"; clientId: string; apiHostname: string };

function WorkOsSession({ expired }: { expired: boolean }) {
  const auth = useAuth();
  const [error, setError] = useState("");
  const latest = useRef(auth);
  latest.current = auth;
  // A change of user, organization or role destroys the old warehouse view.
  // Refreshing a token within that scope preserves forms and pending journals.
  const scope = JSON.stringify([auth.user?.id, auth.organizationId, auth.role]);
  const getAccessToken = useCallback(async () => {
    const token = await latest.current.getAccessToken();
    const claims = getClaims(token);
    if (
      JSON.stringify([
        claims.sub,
        claims.org_id ?? null,
        claims.role ?? null,
      ]) !== scope
    )
      throw new Error(
        "Your warehouse membership changed. Reopen your workspace before continuing.",
      );
    return token;
  }, [scope]);
  const warehouseAuth = useMemo<WarehouseAuth>(
    () => ({
      getAccessToken,
      signOut: () =>
        latest.current.signOut({ returnTo: `${location.origin}/` }),
    }),
    [getAccessToken],
  );
  if (auth.isLoading)
    return (
      <main className="login-form" role="status">
        Checking your sign-in…
      </main>
    );
  if (auth.user && !expired) return <App key={scope} auth={warehouseAuth} />;
  return (
    <main className="login-form">
      <h1>StateSet Warehouse</h1>
      <p>
        {expired
          ? "Your session expired. Sign in again to recover any pending save before repeating physical work."
          : "Sign in with your organization to open your warehouse."}
      </p>
      <button
        className="btn primary"
        onClick={() => {
          setError("");
          void auth
            .signIn()
            .catch(() => setError("Sign-in is unavailable. Please try again."));
        }}
      >
        Sign in with WorkOS
      </button>
      {error && <p role="alert">{error}</p>}
    </main>
  );
}

export default function Authentication() {
  const [config, setConfig] = useState<Config | null>(null);
  const [error, setError] = useState(false);
  const [expired, setExpired] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    const timeout = setTimeout(() => {
      setError(true);
      controller.abort();
    }, 10000);
    void fetch("/auth/config", { signal: controller.signal, cache: "no-store" })
      .then(async (response) => {
        if (!response.ok)
          throw new Error("Authentication configuration unavailable");
        const value = await response.json();
        if (
          value.provider !== "api_key" &&
          (value.provider !== "workos" ||
            !/^client_[a-zA-Z0-9_-]+$/.test(value.clientId) ||
            typeof value.apiHostname !== "string" ||
            new URL(`https://${value.apiHostname}`).host !== value.apiHostname)
        )
          throw new Error("Invalid authentication configuration");
        if (!controller.signal.aborted) {
          if (value.provider === "workos")
            sessionStorage.removeItem("wms-token");
          setConfig(value);
        }
      })
      .catch(() => {
        if (!controller.signal.aborted) setError(true);
      })
      .finally(() => clearTimeout(timeout));
    return () => {
      controller.abort();
      clearTimeout(timeout);
    };
  }, []);
  if (!config)
    return (
      <main className="login-form" role={error ? "alert" : "status"}>
        <h1>StateSet Warehouse</h1>
        <p>
          {error
            ? "Sign-in configuration could not be loaded."
            : "Loading sign-in…"}
        </p>
        {error && (
          <button className="btn" onClick={() => location.reload()}>
            Retry
          </button>
        )}
      </main>
    );
  if (config.provider === "api_key") return <App />;
  return (
    <AuthKitProvider
      clientId={config.clientId}
      apiHostname={config.apiHostname}
      redirectUri={`${location.origin}/`}
      devMode={false}
      onRefreshFailure={() => setExpired(true)}
      onRedirectCallback={() => {
        // Ignore untrusted OAuth state and remove the callback code from history.
        history.replaceState(null, "", "/");
        setExpired(false);
      }}
    >
      <WorkOsSession expired={expired} />
    </AuthKitProvider>
  );
}
