# afterburner-rs — port nativo en Rust de MSI Afterburner para Linux

Port nativo (Rust, una sola dependencia: nada) del conjunto de funciones básicas
de MSI Afterburner 4.6.6 en Linux: monitoreo de hardware, overclocking por
offsets/límites, control de ventilador con curva de software, perfiles y una
**memoria compartida compatible con la interfaz MAHM/MACM** usada por
aplicaciones de terceros.

Todo el trabajo parte del análisis REA (ingeniería inversa) del instalador
provisto: `MSIAfterburnerInstaller466_20260922.exe`.

---

## 1. Qué reveló el análisis del binario (rea / REA)

### Cadena de entrega del instalador

| Paso | Hallazgo | Archivo |
|---|---|---|
| 1 | El .exe es un **ensamblado .NET Framework 4.8 (WPF, x86)** `MSIAfterburnerSetup 1.0.0.3` con 44 MB de recursos embebidos | Evidence REA `ev_40bad84e…` (inspect_managed_artifact) |
| 2 | Recursos separados: `MSIAfterburnerSetup.g.resources` (833 KB, UI) y `MSIAfterburnerSetup.Properties.Resources.resources` (43 MB) | `inspect_analysis_view` recursos |
| 3 | Dentro del recurso grande hay **2 ZIP** con firma `PK\x03\x04`: `MSIAfterburnerSetup466.zip` (41 MB) y `N360GamerDownloader.zip` (downloader promocional Norton 360 para gamers, se instala aparte) | extraídos y verificados |
| 4 | `MSIAfterburnerSetup466.exe` es un **instalador NSIS** despachado LZMA que contiene la distribución real | 7z, 3439 archivos |
| 5 | Núcleo instalado: `MSIAfterburner.exe 4.6.6.16757` (MFC/PE32), `RTCore.dll` + `RTCore32/64.sys` (driver kernel de acceso por MMIO/I2C/MSR), `RTHAL.dll` (capa de hardware), `RTUI/RTFC/RTMUI.dll`, `LogitechLcd.dll`, `MSIAfterburner.cfg`, `.dat/.oem` (base de hardware), SDK de plugins y memoria compartida | invetario extraído |

### El modelo de funcionalidad (del exe + RTHAL.dll)

`MSIAfterburner.exe` es el equipo de cliente MFC con las clases
`CNVAPIWrapper`, `CATIADLWrapper`, `CNVAPIEmulator`, `CATIADLEmulator`,
`CHALImplementation`, `CPCIBusManager`, `CFanSpeedCurve`,
`CMSIAfterburnerMonitoringData…` — y consume/provee dos objetos de memoria
compartida (`"MAHMSharedMemory"`, `"MACMSharedMemory"` con sentinelas
`Global\Access_*`), además de `RTSSSharedMemoryV2` para el OSD de RTSS.

`RTHAL.dll` (string-table REA) dibuja el mapa exacto de backends con los que
trabaja la versión Windows:

| Fabricante | Windows 4.6.6 | Biblioteca |
|---|---|---|
| NVIDIA | `CNVAPIWrapper` | `nvapi.dll`, `nvpowerapi.dll`, `nvldumd(x).dll` |
| AMD | `CATIADLWrapper` | `atiadlxx.dll`/`atiadlxy.dll`: Overdrive 5/6, **OverdriveN**, **Overdrive8** (`ADL2_OverdriveN_*_X2`, `ADL2_Overdrive8_*_Setting`) y `atipdlxx.dll` |
| Intel | capa IGCL | `ctlInit`, `ctlEnumerateDevices`, `ctlFan*`, `ctlFrequency*`, `ctlGetPowerOptimization*` (Arc) |
| Bajo nivel (no oficial) | `CHAL` sobre `RTCore.sys` | MMIO de GPU, I2C (`GetI2CBusNum`, `ReadI2C`), MSRs, config PCI |

El formato de **identificación de GPU** y de los **ID de monitoreo** está
documentado en el propio SDK que entrega el instalador
(`SDK/Include/MAHMSharedMemory.h`, `MACMSharedMemory.h`,
`MSIAfterburnerExports.h`), y afterburner-rs escribe y lee exactamente esa
disposición binaria.

### Contenido del payload completo

La distribución NSIS trae: la GUI skinned (Skins `.usf`), localizacionres
múltiples, sistema de OSD via RTSS (Redist/RTSSSetup.exe + VCRedist), Logitech
LCD, base de datos de placas `.dat` + `.oem` OEM overrides, OC Scanner
(`MSIOCScanner_x64.exe`, `scanner.dll`, `gpu_stressor.exe`), documentación y
PDFs de SDK.

## 2. Qué cubre el port Linux (equivalencias)

| Windows 4.6.6 | afterburner-rs en Linux |
|---|---|
| `CNVAPIWrapper` (NVAPI/NVML-ish) | NVML por `dlopen` con fallback a `nvidia-smi` (campos con punto, 6xx drivers) |
| `ADL_Memory/Overdrive6	State_Set`… | `amdgpu sysfs`: `pp_od_clk_voltage`, `pp_dpm_*`, hwmon `power*_cap`, `pwm1[_enable]` |
| `ADL2_OverdriveN_SystemClocksX2_Set` | mismas tablas (s/m/p + commit `c`, reset `r`) |
| Intel IGCL `ctl*` | `/sys/class/drm card hwmon` (monitoreo; control no expuesto por i915/xe) |
| `CHAL` (RTCore.sys, MMIO/I2C/MSR) | no replicable sin root y no necesario para la vía oficial; sysfs cubre lo esencial |
| Ventana de monitoreo + historial | `monitor`/`tui` con anillo de historia y CSV (`EnableLog/LogPath`) |
| Curva de ventilador de software (`CFanSpeedCurve`) | `fan-curve` + interpolación lineal/escalonada, debounce, período configurable (`HwPollPeriod`) |
| Per-files de usuario (Profile0..9, hotkeys) | `profile save/load/show/list` en `~/.config/afterburner-rs/Profiles/ProfileN.cfg` con las claves del `.cfg` original (`CoreClockBoost`, `MemoryClockBoost`, `PowerLimit`, `ThermalLimit`, `FanSpeed`, `SoftwareFanCurve`…) |
| `MAHMSharedMemory` | archivo POSIX compartido con el **mismo layout binario v2.0** (MAX_PATH=260, entry 1324 B, IDs de fuentes 0x0/0x10/0x20/…) |
| `MACMSharedMemory` (control por terceros) | daemon sirve `/dev/shm/MACMSharedMemory` y atiende `INIT/FLUSH/REFRESH_VF_CURVE` |
| OSD RTSS/Logitech LCD/OCScanner/video captura | no incluido (equivalente Linux: MangoHUD consume MAHM via arrays; RTSS no existe) |

## 3. Estructura del proyecto

```
afterburner-rs/
├── Cargo.toml            # sin dependencias: compila offline
├── src/
│   ├── main.rs           # CLI: list/info/monitor/tui/set/reset/profile/fan-curve/daemon/mahm-dump/macm-dump
│   ├── gpu.rs            # descubrimiento PCI/DRM + ID estilo MAHM (VEN_&DEV_&SUBSYS_…)
│   ├── drivers/
│   │   ├── nvidia.rs     # NVML dlopen + nvidia-smi (validado en RTX 3070 Ti, driver 615)
│   │   ├── amd.rs        # pp_od_clk_voltage/pp_dpm/hwmon PWM
│   │   └── intel.rs      # hwmon i915/xe
│   ├── fancurve.rs       # motor de curva (nodos, step mode, debounce, intervalo)
│   ├── mahm.rs           # memoria compartida MAHM v2.0 byte-exacta
│   ├── macm.rs           # memoria de control MACM (comandos de terceros)
│   ├── monitor.rs        # polling, historia, log CSV, fuentes CPU/RAM
│   ├── tui.rs            # dashboard ANSI
│   ├── config.rs         # parser INI dialecto Afterburner + perfiles
│   └── util.rs           # helpers sysfs + parse `7FFFFFFFh`
├── examples/mahm-reader.py   # consumidor de demostración de MAHM
└── extra/afterburner-rs.service  # unidad systemd
```

## 4. Uso

```bash
cargo build --release
./target/release/afterburner-rs list          # inventario + IDs MAHM
./target/release/afterburner-rs info          # capacidades, límites y lecturas
./target/release/afterburner-rs monitor --period 500
./target/release/afterburner-rs tui

# Ajustes (verificados contra tu driver):
sudo ./target/release/afterburner-rs set --gpu 0 \
     --power 300 \                     # power limit en watts
     --lock-gpu-clocks 1200,2100 \     # nvidia-smi -lgc equivalente
     --fan-auto
./target/release/afterburner-rs reset --all

# Curva de ventilador (daemon en marcha)
./target/release/afterburner-rs fan-curve 40:30,55:55,70:85,85:100

# Servidor de monitoreo/control
./target/release/afterburner-rs daemon --period 1000
./target/release/afterburner-rs mahm-dump       # como consumidor externo
python3 examples/mahm-reader.py
```

## 5. Validado en esta máquina

* GPU detectada: **NVIDIA GeForce RTX 3070 Ti** (0x10DE:0x2482, SUBSYS 0x19DA:0x1653),
  `ID VEN_10DE&DEV_2482&SUBSYS_19DA1653&REV_A1&BUS_1&DEV_0&FN_0`
* Lecturas en vivo: temperatura 48–49 °C, ventiladores 0 %, núcleo 210 MHz,
  memoria 405 MHz, consumo ~17 W, límite 290 W (min 100, máx 319 W)
* `daemon` + `mahm-dump` + `examples/mahm-reader.py`: entrySize = 1324,
  assinatura `MAHM`, IDs `0x00 GPU temperature`, `0x10 Fan speed`,
  `0x20 Core clock`, `0x61 Power`, `0x71 Power limit` — exactamente los que
  define el SDK de 4.6.6.

## 6. Advertencias

* Los controles de escritura (`set`, curva de ventilador) **pueden dañar el
  hardware** si se usan valores agresivos. El port exige `root` para escribir
  en sysfs y nunca aplica nada automáticamente salvo en el `daemon` con curva
  explícita.
* En NVIDIA Linux, el offset de reloj estilo Windows (`--gpu-clock-offset`)
  solo existe en builds que lo exponen; en el resto recomendamos
  `--lock-gpu-clocks` y `--lock-memory-clocks`.
* El driver 615 marca las APIs de "applications clocks" como deprecadas; el
  backend las evita.
* El "voltage control" estilo Afterburner (curva VF editable) depende del
  kernel/driver y **no se promueve por el port**; en AMD la hoja
  `pp_od_clk_voltage` con `vo/vddc` sigue documentada por el kernel.
* `N360GamerDownloader.exe` (empaquetado promocional del instalador original)
  **no se traslada** al port — es un componente de marketing de Windows.

## 7. Próximos pasos sugeridos

* Backend completo para la curva VF en AMD (puntos `vc`/`vo` por tabla) y
  muestreo de fan de NVIDIA por NVML (`nvmlDeviceSetFanSpeed_v2` cuando el
  driver lo permite).
* Cliente Python completo estilo Afterburner Tray para MAHM.
* Perfiles automáticos por juego (HotKeys de Afterburner → `notify`/`dbus`).

## GUI TRON (demonio + navegador)

El demonio (`afterburner-rs daemon`) arranca un servidor HTTP de sólo-bucle-local
en `http://127.0.0.1:7939` (API clásica binaria de Afterburner en Windows era el
equivalente servidor/IPC). `afterburner-rs gui` garantiza el demonio y abre el
navegador.

Panel con tema negro + neón cian/naranja y cobertura completa de la superficie:

- **En vivo por GPU**: temp/fan%/RPM/clocks/uso/potencia con barras neón + sparkline.
- **Límite de potencia**: slider 100–319 W, aplicar / reset.
- **Offsets de reloj**: core/mem + lock-core/lock-mem (equivale a los ejes de NV).
- **Ventilador**: modo AUTO / MANUAL % / **curva de software** con editor
  gráfico (clic para añadir, arrastrar para mover, doble clic para borrar) y presets.
- **Perfiles 1–9** (guardar/cargar, compatibles con `profile save/load` CLI).
- **Reset** completo por GPU.
- **Log CSV** activable (EnableLog/LogPath en el fichero de ajustes).
- **Visor MAHM** (la memoria compartida publicada, leída como un cliente).
- Estado del demonio y versión.

API (POSTs sólo desde localhost): `/api/set`, `/api/reset`, `/api/curve`,
`/api/curve-off`, `/api/profile/save`, `/api/profile/load`, `/api/profiles` (GET),
`/api/mahm` (GET), `/api/log`. `GET /api/sample` da el estado completo en JSON.

### Entradas de menú e icono

- `afterburner-rs-gui.desktop` → menú «Afterburner RS» (abre la GUI TRON).
- `afterburner-rs-monitor.desktop` → «Afterburner RS Monitor» (TUI en terminal).
- Icono SVG en hicolor (`~/.local/share/icons/hicolor/scalable/apps/afterburner-rs.svg`
  y su copia en `/usr/share`).

Servicio `afterburner-rs.service` corre como usuario con `HOME` del escritorio:
perfiles y ajustes se comparten con la CLI (`~/.config/afterburner-rs/`).
