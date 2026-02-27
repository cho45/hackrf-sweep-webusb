const hasOwn = (obj: object, key: PropertyKey) =>
  Object.prototype.hasOwnProperty.call(obj, key);

if (!hasOwn(globalThis, "sampleRate")) {
  Object.defineProperty(globalThis, "sampleRate", {
    configurable: true,
    value: 48_000,
    writable: true,
  });
}

if (!hasOwn(globalThis, "currentTime")) {
  Object.defineProperty(globalThis, "currentTime", {
    configurable: true,
    value: 0,
    writable: true,
  });
}

