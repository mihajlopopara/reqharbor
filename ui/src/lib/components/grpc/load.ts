type GrpcUi = typeof import("./index");
let ui: Promise<GrpcUi> | null = null;

/** The lazy gRPC UI chunk (components, example generator, completion). */
export function loadGrpcUi(): Promise<GrpcUi> {
  return (ui ??= import("./index"));
}
