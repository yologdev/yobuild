# Devices: discovery and use

Draft for review. Not built in yobuild yet. It is shared: any host that runs agents on someone's machine (yobuild, yo) follows the same rules, and yoyo's [`inspect-machine`](https://github.com/yologdev/yoyo-evolve/pull/1046) skill applies them by hand. The words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119.

## 1. Idea

An agent plans better when it knows the machine it runs on. A GPU means a local model is an option; a camera means a vision feature can be tested for real; 8 GB of RAM means no four parallel builds. Without this it guesses or asks.

Two things are kept apart:
- **Listing:** reading the system's device lists, read-only. It tells the agent *what exists*.
- **Using:** opening a device, capturing, writing to it. It always needs the user's approval (§6), and the operating system's where it asks.

Knowing that a camera exists never grants the use of it.

## 2. What is listed

**The machine:** OS and architecture, CPU model and core count, memory, free disk in the workspace.

**Device classes:**

| Class | Means |
|---|---|
| `gpu` | graphics or compute adapters (model, memory when known, backend: Metal, CUDA, Vulkan, DirectX) |
| `npu` | a neural or AI accelerator (Apple Neural Engine, `/dev/accel*`) |
| `camera` | video input |
| `microphone` | audio input |
| `speaker` | audio output |
| `usb` | USB devices (vendor and product, class) |
| `serial` | serial ports and boards (`/dev/ttyUSB*`, `/dev/ttyACM*`, COM ports) |
| `bluetooth` | the controller, and paired devices by kind only |

A new class is added here first, so every host names it the same way.

## 3. The report

```json
{
  "schema": "devices/v0",
  "machine": {"os": "macos", "arch": "aarch64", "cpu": "Apple M2", "cores": 8,
              "memory_bytes": 17179869184, "disk_free_bytes": 128849018880},
  "devices": {
    "gpu": {"state": "available", "items": [{"model": "Apple M2", "backend": "metal"}]},
    "camera": {"state": "not_found"},
    "microphone": {"state": "available", "items": [{"model": "MacBook Pro Microphone"}]},
    "usb": {"state": "available", "items": [{"model": "USB Serial", "vendor_id": "1a86", "product_id": "7523"}]},
    "bluetooth": {"state": "unknown", "reason": "listing needs a permission this process does not have"}
  }
}
```

Each class has one **state**:

| State | Means |
|---|---|
| `available` | listed, at least one found (`items`) |
| `not_found` | listed, none found |
| `unknown` | could not be listed (a command missing, a permission, a container). **Absence is not proof:** an agent says "not found" or "unknown", never "this machine has no camera" |
| `denied` | the user's settings forbid listing this class |

A class a host doesn't list at all is left out of the report.

## 4. Rules for listing

A host MUST:
- **list read-only:** never open a device, capture, write to one, or start a Bluetooth scan;
- **keep nothing identifying:** no serial numbers, MAC or Bluetooth addresses, UUIDs or unique ids, USB location ids, host names, user names, or device names that carry a person's name ("Yuanhao's AirPods" → a placeholder). Class, model, vendor, vendor and product ids, capability and counts stay: they describe a kind of device, not a person;
- **keep the report off the network:** it goes to the agent's own session. A host API that remote clients can read MUST NOT serve the items; at most the state per class, and only to an authenticated client;
- **not persist it:** a report describes one machine at one moment; it is not written to memory, a journal, an issue or a commit.

A host SHOULD:
- list once per session, and again when the user asks (devices come and go);
- list only the classes a task needs, when that is known;
- treat the report as information, never as a grant.

## 5. Where the lists come from

| | macOS | Linux | Windows |
|---|---|---|---|
| machine | `sysctl` | `/proc/cpuinfo`, `/proc/meminfo` | WMI |
| `gpu` | `system_profiler -json SPDisplaysDataType` | `/sys/class/drm`, `lspci`, `nvidia-smi` | `Win32_VideoController` |
| `npu` | the chip model | `/dev/accel*` | vendor tools |
| `camera` | `SPCameraDataType` | `/dev/video*` | `Get-PnpDevice -Class Camera` |
| `microphone`, `speaker` | `SPAudioDataType` | `/proc/asound/cards` | `AudioEndpoint` devices |
| `usb` | `SPUSBDataType` | `/sys/bus/usb`, `lsusb` | `Get-PnpDevice -Class USB` |
| `serial` | `/dev/cu.*` | `/dev/ttyUSB*`, `/dev/ttyACM*` | COM ports |

Listing generally doesn't trigger the operating system's permission prompts; opening a camera or microphone does. Measured on a Mac mini (M2): one `system_profiler` call for four classes takes about 2 s; it returned `serial_num` and `location_id` fields, which the redaction removes.

**CI runners are virtual machines.** A report from GitHub Actions describes the runner (no camera, no GPU, a few cores), never the user's machine.

## 6. Using a device

- **Each class is its own grant** (`camera`, `microphone`, `gpu`, `usb`, `serial`, `bluetooth`), approved by the user per agent or app, like any capability. Listing (`devices`) is a separate, lighter grant.
- **The approval names the purpose**, and an agent asks again when the purpose changes.
- **The operating system may ask too** (macOS camera and microphone prompts). A host never works around it.
- **A cloud run has no devices.** A Cloudflare Worker has none of these classes; an agent or app that needs one runs locally.

## 7. Per host

- **yobuild:** how `agent.toml` asks for a device class, and where its approval is shown, is not designed yet. It belongs in [Design](design.md) once a plugin or built-in needs a device.
- **yo:** its app spec names these classes as capabilities an app can require, and checks them against this report before a run.
- **yoyo:** the `inspect-machine` skill lists by hand with `bash`, under these rules.

## 8. Not covered

- Drivers, hot-plug events, and devices on other machines.
- Which device to pick when several of a class exist (a host or agent chooses, then asks).
- A shared library for the listing. Each host lists its own way until a second implementation exists.
