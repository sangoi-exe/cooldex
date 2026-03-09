import { AppServerTransport } from "./transport.js";

export function createAppServerClient({ config, logger }) {
  const transport = new AppServerTransport({ config, logger });

  return {
    start() {
      return transport.start();
    },
    stop() {
      return transport.stop();
    },
    onNotification(listener) {
      transport.on("notification", listener);
      return () => transport.off("notification", listener);
    },
    onServerRequest(listener) {
      transport.on("serverRequest", listener);
      return () => transport.off("serverRequest", listener);
    },
    onFatal(listener) {
      transport.on("fatal", listener);
      return () => transport.off("fatal", listener);
    },
    respondResult(id, result) {
      transport.respondResult(id, result);
    },
    respondError(id, error) {
      transport.respondError(id, error);
    },
    threadStart(params) {
      return transport.request("thread/start", params);
    },
    turnStart(params) {
      return transport.request("turn/start", params);
    },
  };
}
