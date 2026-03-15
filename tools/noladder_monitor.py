#!/usr/bin/python3
# noladder_monitor.py

import sys
import mmap
import struct
import os
import tomllib
import time
from pathlib import Path
from collections import deque, defaultdict

from PySide6.QtWidgets import (
    QApplication, QMainWindow,
    QWidget, QVBoxLayout, QHBoxLayout,
    QLabel, QTableWidget, QTableWidgetItem,
    QGroupBox, QSplitter, QHeaderView,
    QStatusBar, QLineEdit, QScrollArea,
    QDialog, QSpinBox, QDoubleSpinBox,
    QCheckBox, QPushButton, QMenu,
)
from PySide6.QtCore import (
    Qt, QTimer, Signal, QThread, QSize
)
from PySide6.QtGui import QColor, QFont
from PySide6.QtCharts import QChart, QChartView, QLineSeries
from PySide6.QtCore import QPointF

# ------------------------------------
# Constants - must match Rust layout
# ------------------------------------

VALUE_SIZE   = 8
MAX_IO       = 4096
SEQUENCE_OFF = 0
INPUTS_OFF   = 8
OUTPUTS_OFF  = INPUTS_OFF + (MAX_IO * VALUE_SIZE)

TAG_UNSET = 0
TAG_BOOL  = 1
TAG_INT   = 2
TAG_FLOAT = 3

# ------------------------------------
# Symbol table constants
# must match Rust repr(C) layout
# ------------------------------------

SYMBOLS_PATH      = "/dev/shm/noladder_symbols"
MAX_SYMBOLS_COUNT = 512

# Symbol struct (repr(C), size=72):
#   u32  index      offset 0  (4 bytes)
#   u8   kind       offset 4  (1 byte)  0=input 1=output
#   u8   value_type offset 5  (1 byte)  0=unset 1=bool 2=int 3=float
#   u8   name_len   offset 6  (1 byte)
#   u8   _pad       offset 7  (1 byte)
#   u8   name[64]   offset 8  (64 bytes)
_SYMBOL_SIZE        = 72
_TABLE_HEADER_SIZE  = 8  # u32 count + [u8; 4] padding

# ------------------------------------
# Shared memory reader
# ------------------------------------

class ShmReader:
    def __init__(self, path="/dev/shm/noladder_io"):
        self.path = path
        self.mm   = None
        self.open()

    def open(self):
        try:
            fd       = os.open(
                self.path, os.O_RDONLY
            )
            size     = os.fstat(fd).st_size
            self.mm  = mmap.mmap(
                fd, size,
                mmap.MAP_SHARED,
                mmap.PROT_READ,
            )
            os.close(fd)
            return True
        except Exception as e:
            self.mm = None
            return False

    def is_open(self):
        return self.mm is not None

    def sequence(self):
        if not self.mm:
            return 0
        self.mm.seek(SEQUENCE_OFF)
        return struct.unpack(
            "Q", self.mm.read(8)
        )[0]

    def read_value(self, offset):
        if not self.mm:
            return None
        try:
            self.mm.seek(offset)
            data = self.mm.read(VALUE_SIZE)
            tag  = struct.unpack(
                "I", data[0:4]
            )[0]

            if tag == TAG_BOOL:
                return bool(data[4])
            elif tag == TAG_INT:
                return struct.unpack(
                    "i", data[4:8]
                )[0]
            elif tag == TAG_FLOAT:
                val = struct.unpack(
                    "f", data[4:8]
                )[0]
                # filter NaN/inf
                if val != val:
                    return None
                return round(val, 3)
            else:
                return None
        except Exception:
            return None

    def read_input(self, index):
        return self.read_value(
            INPUTS_OFF + index * VALUE_SIZE
        )

    def read_output(self, index):
        return self.read_value(
            OUTPUTS_OFF + index * VALUE_SIZE
        )

    def read_all_inputs(self, count):
        return [
            self.read_input(i)
            for i in range(count)
        ]

    def read_all_outputs(self, count):
        return [
            self.read_output(i)
            for i in range(count)
        ]

    def read_snapshot(self, input_count, output_count):
        """
        Read a consistent snapshot of inputs and outputs.
        Returns dict with seq/inputs/outputs or None if race detected.
        """
        if not self.mm:
            return None
        try:
            seq1 = self.sequence()
            inputs = self.read_all_inputs(input_count)
            outputs = self.read_all_outputs(output_count)
            seq2 = self.sequence()

            if seq1 != seq2:
                return None  # race detected

            return {
                "seq": seq1,
                "inputs": inputs,
                "outputs": outputs,
            }
        except Exception:
            return None

# ------------------------------------
# Force store
# writes force commands to separate mmap
# ------------------------------------

class ForceStore:
    """
    Manages output forcing via /dev/shm/noladder_force.
    Format: MAX_IO entries of 9 bytes each:
      - 1 byte: flags (bit 0 = active)
      - 8 bytes: IoValue (tag 4 bytes + value 4 bytes)
    """
    FORCE_PATH = "/dev/shm/noladder_force"
    ENTRY_SIZE = 9  # 1 byte flags + 8 byte IoValue

    def __init__(self):
        self.mm = None
        self.open()

    def open(self):
        try:
            # Try to open existing file
            if os.path.exists(self.FORCE_PATH):
                fd = os.open(
                    self.FORCE_PATH,
                    os.O_RDWR
                )
            else:
                # Create if missing
                fd = os.open(
                    self.FORCE_PATH,
                    os.O_CREAT | os.O_RDWR,
                    0o666
                )
                os.ftruncate(fd, MAX_IO * self.ENTRY_SIZE)

            size = os.fstat(fd).st_size
            self.mm = mmap.mmap(
                fd, size,
                mmap.MAP_SHARED,
                mmap.PROT_READ | mmap.PROT_WRITE,
            )
            os.close(fd)
            return True
        except Exception as e:
            self.mm = None
            return False

    def set_force(self, index, tag, value):
        """Set forced value for output slot."""
        if not self.mm or index >= MAX_IO:
            return False
        try:
            offset = index * self.ENTRY_SIZE
            # flags byte (bit 0 = active)
            flags = 1
            # value: 8 bytes (tag 4 + value 4)
            packed = struct.pack(
                "Bi4f",
                flags,
                tag,
                struct.unpack("f", struct.pack("I", value))[0]
            )
            self.mm.seek(offset)
            self.mm.write(packed)
            return True
        except Exception:
            return False

    def clear_force(self, index):
        """Clear forced value for output slot."""
        if not self.mm or index >= MAX_IO:
            return False
        try:
            offset = index * self.ENTRY_SIZE
            flags = 0
            self.mm.seek(offset)
            self.mm.write(struct.pack("B", flags))
            return True
        except Exception:
            return False

    def get_force(self, index):
        """
        Get forced value if active, else None.
        Returns (tag, value) tuple or None.
        """
        if not self.mm or index >= MAX_IO:
            return None
        try:
            offset = index * self.ENTRY_SIZE
            self.mm.seek(offset)
            data = self.mm.read(self.ENTRY_SIZE)
            flags = struct.unpack("B", data[0:1])[0]

            if not (flags & 1):  # not active
                return None

            tag = struct.unpack("I", data[1:5])[0]
            val = struct.unpack("f", data[5:9])[0]
            return (tag, val)
        except Exception:
            return None

# ------------------------------------
# Symbol table reader
# ------------------------------------

def load_symbols(path=SYMBOLS_PATH):
    """
    Read the shared symbol table written by noladder-bus.

    Returns dict of (kind, index) -> (name, kind, value_type):
        kind:       0 = input, 1 = output
        index:      slot index within inputs or outputs array
        value_type: 0 = unset, 1 = bool, 2 = int, 3 = float

    Returns empty dict if the symbol table is not available
    or contains no entries.
    """
    try:
        fd   = os.open(path, os.O_RDONLY)
        size = os.fstat(fd).st_size
        mm   = mmap.mmap(
            fd, size,
            mmap.MAP_SHARED,
            mmap.PROT_READ,
        )
        os.close(fd)

        mm.seek(0)
        count = struct.unpack("I", mm.read(4))[0]

        if count == 0 or count > MAX_SYMBOLS_COUNT:
            mm.close()
            return {}

        result = {}
        for i in range(count):
            offset = _TABLE_HEADER_SIZE + i * _SYMBOL_SIZE
            mm.seek(offset)
            data       = mm.read(_SYMBOL_SIZE)
            index      = struct.unpack_from("I", data, 0)[0]
            kind       = data[4]
            value_type = data[5]
            name_len   = data[6]
            name       = data[8:8 + name_len].decode(
                "utf-8", errors="replace"
            )
            result[(kind, index)] = (name, kind, value_type)

        mm.close()
        return result
    except Exception:
        return {}

# ------------------------------------
# Config reader
# loads machine.toml to get device names
# ------------------------------------

class MachineConfig:
    def __init__(self, path="machine.toml"):
        self.devices      = []
        self.symbol_source = False  # True = live table, False = toml
        self.load(path)

    def load(self, path):
        # try live symbol table first
        symbols = load_symbols()
        if symbols:
            self._build_from_symbols(symbols)
            self.symbol_source = True
            return

        # fall back to machine.toml
        self.symbol_source = False
        try:
            with open(path, "rb") as f:
                config = tomllib.load(f)

            # build sorted device list
            # same order as Rust loader
            devices_raw = config.get(
                "device", {}
            )
            sorted_devices = sorted(
                devices_raw.items()
            )

            input_cursor  = 0
            output_cursor = 0

            for path, device in sorted_devices:
                kind          = device.get(
                    "type", "unknown"
                )
                input_count   = self.input_count(
                    kind
                )
                output_count  = self.output_count(
                    kind
                )

                self.devices.append({
                    "path":         path,
                    "kind":         kind,
                    "bus":          device.get(
                        "bus", ""
                    ),
                    "node":         device.get(
                        "node", 0
                    ),
                    "input_base":   input_cursor,
                    "output_base":  output_cursor,
                    "input_count":  input_count,
                    "output_count": output_count,
                    "note":         device.get(
                        "note", None
                    ),
                })

                input_cursor  += input_count
                output_cursor += output_count

        except Exception as e:
            print(f"Config load error: {e}")

    def _build_from_symbols(self, symbols):
        """
        Reconstruct device list from a symbol table dict.

        symbols: {(kind, index): (name, kind, value_type)}
        Groups symbols by device path prefix to form device panels.
        """
        input_sigs  = defaultdict(list)
        output_sigs = defaultdict(list)

        for (kind, index), (name, _, _vtype) in symbols.items():
            # split "device.path.signal" into prefix + signal
            if "." in name:
                device_path, signal = name.rsplit(".", 1)
            else:
                device_path = name
                signal      = ""

            if kind == 0:
                input_sigs[device_path].append((index, signal))
            else:
                output_sigs[device_path].append((index, signal))

        all_paths = sorted(
            set(list(input_sigs.keys()) +
                list(output_sigs.keys()))
        )

        for dpath in all_paths:
            ins  = sorted(input_sigs[dpath],  key=lambda x: x[0])
            outs = sorted(output_sigs[dpath], key=lambda x: x[0])

            self.devices.append({
                "path":           dpath,
                "kind":           "unknown",
                "bus":            "",
                "node":           0,
                "input_base":     ins[0][0]  if ins  else 0,
                "output_base":    outs[0][0] if outs else 0,
                "input_count":    len(ins),
                "output_count":   len(outs),
                "note":           None,
                "input_signals":  [s for _, s in ins],
                "output_signals": [s for _, s in outs],
            })

    def input_count(self, kind):
        counts = {
            "servo_drive": 10,
            "vfd":          2,
            "digital_in":   8,
            "digital_out":  0,
            "analog_in":    4,
            "analog_out":   0,
            "mixed_io":     4,
            "safety_relay": 2,
            "safety_door":  2,
        }
        return counts.get(kind, 4)

    def output_count(self, kind):
        counts = {
            "servo_drive": 6,
            "vfd":          2,
            "digital_in":   0,
            "digital_out":  8,
            "analog_in":    0,
            "analog_out":   4,
            "mixed_io":     4,
            "safety_relay": 1,
            "safety_door":  0,
        }
        return counts.get(kind, 2)

    def input_signals(self, kind):
        signals = {
            "servo_drive": [
                "actual_position",
                "actual_velocity",
                "actual_torque",
                "following_error",
                "enabled",
                "fault",
                "target_reached",
                "homing_complete",
                "error_code",
                "referenced",
            ],
            "vfd": [
                "speed",
                "current",
            ],
            "digital_in": [
                "0","1","2","3",
                "4","5","6","7",
            ],
            "analog_in": [
                "0","1","2","3",
            ],
            "safety_relay": ["ok", "fault"],
            "safety_door":  ["closed","locked"],
        }
        default = [
            str(i) for i in range(
                self.input_count(kind)
            )
        ]
        return signals.get(kind, default)

    def output_signals(self, kind):
        signals = {
            "servo_drive": [
                "target_position",
                "target_velocity",
                "target_torque",
                "max_torque",
                "fault_reset",
                "quick_stop",
            ],
            "vfd": [
                "setpoint",
                "enable",
            ],
            "digital_out": [
                "0","1","2","3",
                "4","5","6","7",
            ],
            "analog_out": [
                "0","1","2","3",
            ],
            "safety_relay": ["reset"],
        }
        default = [
            str(i) for i in range(
                self.output_count(kind)
            )
        ]
        return signals.get(kind, default)

# ------------------------------------
# Signal history (for plotting)
# ------------------------------------

class SignalHistory:
    """Circular deque of (timestamp, value) for each signal."""

    def __init__(self, max_samples=200):
        self.max_samples = max_samples
        self.histories = {}  # key -> deque

    def record(self, key, timestamp, value):
        """Add sample to history."""
        if key not in self.histories:
            self.histories[key] = deque(maxlen=self.max_samples)
        self.histories[key].append((timestamp, value))

    def get_history(self, key):
        """Get all samples for a key."""
        return self.histories.get(key, deque())

# ------------------------------------
# Value display widget
# colored by type
# ------------------------------------

# ------------------------------------
# Force dialog
# ------------------------------------

class ForceDialog(QDialog):
    def __init__(self, signal_name, current_value, parent=None):
        super().__init__(parent)
        self.setWindowTitle(f"Force {signal_name}")
        self.signal_name = signal_name
        self.forced_value = None

        layout = QVBoxLayout(self)

        # Display current value
        info = QLabel(f"Signal: {signal_name}\nCurrent: {current_value}")
        layout.addWidget(info)

        # Input field based on type
        input_layout = QHBoxLayout()
        input_layout.addWidget(QLabel("Force value:"))

        if isinstance(current_value, bool):
            # Bool: checkbox or selection
            self.input_widget = QCheckBox()
            self.input_widget.setChecked(
                current_value if current_value else False
            )
        elif isinstance(current_value, int):
            self.input_widget = QSpinBox()
            self.input_widget.setRange(-2147483648, 2147483647)
            if current_value is not None:
                self.input_widget.setValue(current_value)
        else:  # float
            self.input_widget = QDoubleSpinBox()
            self.input_widget.setRange(-1e9, 1e9)
            self.input_widget.setDecimals(3)
            if current_value is not None:
                self.input_widget.setValue(float(current_value))

        input_layout.addWidget(self.input_widget)
        layout.addLayout(input_layout)

        # Buttons
        btn_layout = QHBoxLayout()
        ok_btn = QPushButton("Force")
        cancel_btn = QPushButton("Cancel")
        ok_btn.clicked.connect(self.accept)
        cancel_btn.clicked.connect(self.reject)
        btn_layout.addWidget(ok_btn)
        btn_layout.addWidget(cancel_btn)
        layout.addLayout(btn_layout)

    def get_value(self):
        """Get the forced value."""
        if isinstance(self.input_widget, QSpinBox):
            return self.input_widget.value()
        elif isinstance(self.input_widget, QDoubleSpinBox):
            return self.input_widget.value()
        else:
            # Checkbox
            return self.input_widget.isChecked()

# ------------------------------------
# Value display widget
# colored by type
# ------------------------------------

def make_value_item(value):
    if value is None:
        item = QTableWidgetItem("—")
        item.setForeground(
            QColor(128, 128, 128)
        )
    elif isinstance(value, bool):
        item = QTableWidgetItem(
            "TRUE" if value else "false"
        )
        item.setForeground(
            QColor(0, 200, 100)
            if value else
            QColor(180, 180, 180)
        )
    elif isinstance(value, int):
        item = QTableWidgetItem(str(value))
        item.setForeground(
            QColor(100, 180, 255)
        )
    elif isinstance(value, float):
        item = QTableWidgetItem(f"{value:.3f}")
        item.setForeground(
            QColor(255, 200, 100)
        )
    else:
        item = QTableWidgetItem(str(value))

    item.setTextAlignment(
        Qt.AlignRight | Qt.AlignVCenter
    )
    item.setFlags(
        Qt.ItemIsEnabled | Qt.ItemIsSelectable
    )
    return item

# ------------------------------------
# Plot window
# displays signal history with QChart
# ------------------------------------

class PlotWindow(QWidget):
    def __init__(self, key, history, parent=None):
        super().__init__(parent)
        self.key = key
        self.history = history
        self.setWindowTitle(f"Plot: {key}")
        self.resize(800, 400)

        layout = QVBoxLayout(self)

        # Create chart
        self.chart = QChart()
        self.chart.setTitle(f"Signal: {key}")
        self.chart.setAnimationOptions(
            QChart.NoAnimation
        )

        self.chart_view = QChartView(self.chart)
        self.chart_view.setRenderHint(
            self.chart_view.RenderHint.Antialiasing
        )
        layout.addWidget(self.chart_view)

        # Timer for updates
        self.plot_timer = QTimer()
        self.plot_timer.timeout.connect(self._refresh)
        self.plot_timer.start(500)  # 2 Hz

    def _refresh(self):
        """Update plot from history."""
        hist = self.history.get_history(self.key)
        if not hist:
            return

        # Clear old series
        self.chart.removeAllSeries()

        # Create new series
        series = QLineSeries()
        for idx, (ts, val) in enumerate(hist):
            if val is not None:
                series.append(QPointF(idx, float(val)))

        self.chart.addSeries(series)
        self.chart.createDefaultAxes()
        self.chart.axisX().setTitleText("Sample")
        self.chart.axisY().setTitleText("Value")

    def closeEvent(self, event):
        self.plot_timer.stop()
        super().closeEvent(event)

# ------------------------------------
# Device panel
# shows inputs and outputs for one device
# ------------------------------------

class DevicePanel(QGroupBox):
    def __init__(self, device, history=None, force_store=None, parent=None):
        title = f"{device['path']}  " \
                f"({device['kind']}  " \
                f"{device['bus']}  " \
                f"node {device['node']})"
        super().__init__(title, parent)

        self.device       = device
        self.history      = history
        self.force_store  = force_store
        self.input_rows   = []
        self.output_rows  = []
        self.plot_windows = {}

        layout = QHBoxLayout(self)

        # inputs table
        if device["input_count"] > 0:
            in_group  = QGroupBox("Inputs")
            in_layout = QVBoxLayout(in_group)
            self.in_table = self._make_table(
                device["input_count"]
            )
            signals = device.get(
                "input_signals", []
            )
            for i, sig in enumerate(signals):
                self.in_table.setItem(
                    i, 0,
                    QTableWidgetItem(sig)
                )
                self.in_table.setItem(
                    i, 1,
                    make_value_item(None)
                )
            self.in_table.cellDoubleClicked.connect(
                self._on_input_double_click
            )
            in_layout.addWidget(self.in_table)
            layout.addWidget(in_group)

        # outputs table
        if device["output_count"] > 0:
            out_group  = QGroupBox("Outputs")
            out_layout = QVBoxLayout(out_group)
            self.out_table = self._make_table(
                device["output_count"]
            )
            signals = device.get(
                "output_signals", []
            )
            for i, sig in enumerate(signals):
                self.out_table.setItem(
                    i, 0,
                    QTableWidgetItem(sig)
                )
                self.out_table.setItem(
                    i, 1,
                    make_value_item(None)
                )
            self.out_table.cellDoubleClicked.connect(
                self._on_output_double_click
            )
            self.out_table.setContextMenuPolicy(
                Qt.CustomContextMenu
            )
            self.out_table.customContextMenuRequested.connect(
                self._on_output_context_menu
            )
            out_layout.addWidget(self.out_table)
            layout.addWidget(out_group)

        if device.get("note"):
            note = QLabel(
                f"⚠ {device['note']}"
            )
            note.setStyleSheet(
                "color: orange; font-style: italic;"
            )
            layout.addWidget(note)

    def _make_table(self, rows):
        t = QTableWidget(rows, 2)
        t.setHorizontalHeaderLabels(
            ["Signal", "Value"]
        )
        t.horizontalHeader().setSectionResizeMode(
            0, QHeaderView.Stretch
        )
        t.horizontalHeader().setSectionResizeMode(
            1, QHeaderView.ResizeToContents
        )
        t.verticalHeader().setVisible(False)
        t.setAlternatingRowColors(True)
        t.setRowHeight(0, 22)  # reduced row height
        for i in range(rows):
            t.setRowHeight(i, 22)
        return t

    def update_inputs(self, values, timestamp=None):
        if not hasattr(self, "in_table"):
            return
        for i, val in enumerate(values):
            # Record in history
            if self.history and timestamp:
                key = f"{self.device['path']}.{self.device['input_signals'][i]}"
                self.history.record(key, timestamp, val)

            self.in_table.setItem(
                i, 1, make_value_item(val)
            )

    def update_outputs(self, values, timestamp=None):
        if not hasattr(self, "out_table"):
            return
        for i, val in enumerate(values):
            # Record in history
            if self.history and timestamp:
                key = f"{self.device['path']}.{self.device['output_signals'][i]}"
                self.history.record(key, timestamp, val)

            # Check for forced value
            forced = None
            if self.force_store:
                forced = self.force_store.get_force(
                    self.device["output_base"] + i
                )

            # Create item
            if forced:
                # Show forced value with star prefix
                tag, fval = forced
                item = QTableWidgetItem(f"★ {fval}")
                item.setBackground(
                    QColor(255, 140, 0)
                )
                item.setTextAlignment(
                    Qt.AlignRight | Qt.AlignVCenter
                )
                item.setFlags(
                    Qt.ItemIsEnabled | Qt.ItemIsSelectable
                )
                self.out_table.setItem(i, 1, item)
            else:
                self.out_table.setItem(
                    i, 1, make_value_item(val)
                )

    def _on_input_double_click(self, row, col):
        """Open plot for input signal."""
        if col != 1:
            return
        key = f"{self.device['path']}.{self.device['input_signals'][row]}"
        if not self.history:
            return
        if key not in self.plot_windows:
            window = PlotWindow(key, self.history, self)
            self.plot_windows[key] = window
        self.plot_windows[key].show()
        self.plot_windows[key].raise_()

    def _on_output_double_click(self, row, col):
        """Open plot for output signal."""
        if col != 1:
            return
        key = f"{self.device['path']}.{self.device['output_signals'][row]}"
        if not self.history:
            return
        if key not in self.plot_windows:
            window = PlotWindow(key, self.history, self)
            self.plot_windows[key] = window
        self.plot_windows[key].show()
        self.plot_windows[key].raise_()

    def _on_output_context_menu(self, pos):
        """Right-click context menu for outputs."""
        item = self.out_table.itemAt(pos)
        if not item:
            return

        row = self.out_table.row(item)
        if self.out_table.column(item) != 1:
            return

        menu = QMenu(self)
        force_action = menu.addAction("Force value…")
        clear_action = menu.addAction("Clear force")

        action = menu.exec(
            self.out_table.mapToGlobal(pos)
        )

        if action == force_action:
            self._force_output(row)
        elif action == clear_action:
            self._clear_force(row)

    def _force_output(self, row):
        """Open force dialog for output."""
        current_val = None
        if self.out_table.item(row, 1):
            text = self.out_table.item(row, 1).text()
            # Try to parse
            try:
                if "true" in text.lower():
                    current_val = True
                elif "false" in text.lower():
                    current_val = False
                else:
                    current_val = float(text.replace("★ ", ""))
            except:
                current_val = 0

        signal_name = self.device['output_signals'][row]
        dlg = ForceDialog(signal_name, current_val, self)
        if dlg.exec():
            val = dlg.get_value()
            if self.force_store:
                # Determine tag
                if isinstance(val, bool):
                    tag = TAG_BOOL
                    packed_val = val
                elif isinstance(val, int):
                    tag = TAG_INT
                    packed_val = val
                else:
                    tag = TAG_FLOAT
                    packed_val = struct.pack(
                        "f", float(val)
                    )

                self.force_store.set_force(
                    self.device["output_base"] + row,
                    tag,
                    packed_val
                )

    def _clear_force(self, row):
        """Clear force for output."""
        if self.force_store:
            self.force_store.clear_force(
                self.device["output_base"] + row
            )

# ------------------------------------
# Status panel
# Mock runtime state display
# ------------------------------------

class StatusPanel(QGroupBox):
    def __init__(self, parent=None):
        super().__init__("Runtime", parent)

        layout = QHBoxLayout(self)

        self.rung_label = QLabel("Rung: main")
        layout.addWidget(self.rung_label)

        self.state_label = QLabel("State: RUNNING")
        self.state_label.setStyleSheet(
            "color: #00c864; font-weight: bold;"
        )
        layout.addWidget(self.state_label)

        self.waiting_label = QLabel("Waiting: —")
        layout.addWidget(self.waiting_label)

        self.wake_label = QLabel("Last wake: —")
        layout.addWidget(self.wake_label)

        layout.addStretch()

    def update(self, data):
        """Update with mock runtime data."""
        if "rung" in data:
            self.rung_label.setText(f"Rung: {data['rung']}")
        if "state" in data:
            state = data['state']
            self.state_label.setText(f"State: {state}")
            if "RUNNING" in state:
                self.state_label.setStyleSheet(
                    "color: #00c864; font-weight: bold;"
                )
            elif "WAITING" in state:
                self.state_label.setStyleSheet(
                    "color: #ffcc00; font-weight: bold;"
                )
            else:
                self.state_label.setStyleSheet(
                    "color: #ff6b6b; font-weight: bold;"
                )
        if "waiting" in data:
            self.waiting_label.setText(
                f"Waiting: {data['waiting']}"
            )
        if "last_wake" in data:
            self.wake_label.setText(
                f"Last wake: {data['last_wake']}"
            )

    def attach_shm(self, path):
        """Stub: attach to runtime state in shm."""
        print(f"attach_shm({path}) not yet implemented")

# ------------------------------------
# Main window
# ------------------------------------

class MonitorWindow(QMainWindow):
    def __init__(
        self,
        shm_path    = "/dev/shm/noladder_io",
        config_path = "machine.toml",
    ):
        super().__init__()
        self.setWindowTitle(
            "NoLadder Monitor"
        )
        self.resize(1200, 800)
        self.shm_path    = shm_path
        self.config_path = config_path

        self.shm          = ShmReader(self.shm_path)
        self.config       = MachineConfig(config_path)
        self.history      = SignalHistory(max_samples=200)
        self.force_store  = ForceStore()

        self._build_ui()
        self._start_timer()

        self.last_sequence = 0
        self.cycle_count   = 0
        self.stale_count   = 0
        self.last_seq_time = time.time()

    def _build_ui(self):
        central = QWidget()
        self.setCentralWidget(central)
        layout  = QVBoxLayout(central)

        # header with status, seq, rate, counts
        header = QHBoxLayout()

        self.status_label = QLabel(
            "⬤ Connecting..."
        )
        self.status_label.setStyleSheet(
            "color: orange; font-weight: bold;"
        )
        header.addWidget(self.status_label)

        header.addSpacing(10)

        self.seq_label = QLabel("Sequence: 0")
        header.addWidget(self.seq_label)

        header.addSpacing(10)

        self.rate_label = QLabel("0 Hz")
        header.addWidget(self.rate_label)

        header.addSpacing(20)

        # Count total signals
        total_inputs = sum(
            d.get("input_count", 0)
            for d in self.config.devices
        )
        total_outputs = sum(
            d.get("output_count", 0)
            for d in self.config.devices
        )

        self.count_label = QLabel(
            f"devices: {len(self.config.devices)} | "
            f"signals: {total_inputs + total_outputs}"
        )
        header.addWidget(self.count_label)

        header.addSpacing(20)

        # show whether symbols came from bus or config file
        if self.config.symbol_source:
            src_text  = "⬤ Live symbol table"
            src_style = "color: #00c864;"
        else:
            src_text  = "⬤ Config file"
            src_style = "color: #9cdcfe;"
        self.source_label = QLabel(src_text)
        self.source_label.setStyleSheet(src_style)
        header.addWidget(self.source_label)

        header.addStretch()
        layout.addLayout(header)

        # Search bar
        self.search_bar = QLineEdit()
        self.search_bar.setPlaceholderText(
            "Filter devices / signals…"
        )
        self.search_bar.textChanged.connect(
            self._apply_filter
        )
        layout.addWidget(self.search_bar)

        # Status panel
        self.status_panel = StatusPanel()
        self.status_panel.update({
            "rung": "main",
            "state": "RUNNING",
            "waiting": "—",
            "last_wake": "—",
        })
        layout.addWidget(self.status_panel)

        # device panels in scroll area
        scroll_content = QWidget()
        scroll_layout = QVBoxLayout(scroll_content)

        for device in self.config.devices:
            # attach signal names from device kind when
            # loading from toml — symbol table path sets
            # them directly in _build_from_symbols
            if not self.config.symbol_source:
                device["input_signals"] = \
                    self.config.input_signals(
                        device["kind"]
                    )
                device["output_signals"] = \
                    self.config.output_signals(
                        device["kind"]
                    )

            panel = DevicePanel(
                device,
                history=self.history,
                force_store=self.force_store
            )
            scroll_layout.addWidget(panel)
            device["panel"] = panel

        scroll_layout.addStretch()

        scroll_area = QScrollArea()
        scroll_area.setWidget(scroll_content)
        scroll_area.setWidgetResizable(True)
        layout.addWidget(scroll_area)

        # status bar
        self.statusBar().showMessage(
            f"Monitoring {self.shm_path}"
        )

    def _apply_filter(self, text):
        """Filter device panels by text."""
        text_lower = text.lower()
        visible_count = 0

        for device in self.config.devices:
            panel = device.get("panel")
            if not panel:
                continue

            # Check if device path or any signal matches
            match = (
                text_lower in device["path"].lower()
            )

            if not match:
                # Check input signals
                for sig in device.get(
                    "input_signals", []
                ):
                    if text_lower in sig.lower():
                        match = True
                        break

            if not match:
                # Check output signals
                for sig in device.get(
                    "output_signals", []
                ):
                    if text_lower in sig.lower():
                        match = True
                        break

            panel.setVisible(match)
            if match:
                visible_count += 1

        self.count_label.setText(
            f"devices: {visible_count} | "
            f"signals: (filtered)"
        )

    def _start_timer(self):
        self.timer = QTimer()
        self.timer.timeout.connect(self._update)
        # update at 30Hz — fast enough to see
        # slow enough to not stress the CPU
        self.timer.start(33)

        # rate counter
        self.rate_timer = QTimer()
        self.rate_timer.timeout.connect(
            self._update_rate
        )
        self.rate_timer.start(1000)
        self.updates_this_second = 0

    def _update(self):
        # reconnect if needed
        if not self.shm.is_open():
            if not self.shm.open():
                self.status_label.setText(
                    "⬤ Waiting for bus server..."
                )
                self.status_label.setStyleSheet(
                    "color: orange; "
                    "font-weight: bold;"
                )
                return

        # Total input/output counts
        total_inputs = sum(
            d.get("input_count", 0)
            for d in self.config.devices
        )
        total_outputs = sum(
            d.get("output_count", 0)
            for d in self.config.devices
        )

        # Read snapshot
        snap = self.shm.read_snapshot(
            total_inputs, total_outputs
        )

        if snap is None:
            # Race condition detected
            self.stale_count += 1
            self.statusBar().showMessage(
                f"Monitoring {self.shm_path} | "
                f"stale: {self.stale_count}"
            )
            return

        seq = snap["seq"]
        timestamp = time.time()

        if seq != self.last_sequence:
            self.last_sequence = seq
            self.updates_this_second += 1

            # update all device panels
            input_offset = 0
            output_offset = 0

            for device in self.config.devices:
                panel = device.get("panel")
                if not panel:
                    continue

                input_count = device["input_count"]
                output_count = device["output_count"]

                # Extract inputs and outputs
                inputs = snap["inputs"][
                    input_offset:input_offset + input_count
                ]
                outputs = snap["outputs"][
                    output_offset:output_offset + output_count
                ]

                input_offset += input_count
                output_offset += output_count

                panel.update_inputs(inputs, timestamp)
                panel.update_outputs(outputs, timestamp)

            self.seq_label.setText(
                f"Sequence: {seq:,}"
            )
            self.status_label.setText(
                "⬤ Running"
            )
            self.status_label.setStyleSheet(
                "color: #00c864; "
                "font-weight: bold;"
            )

            # Check for stale sequence
            now = time.time()
            if now - self.last_seq_time > 2.0:
                self.status_label.setText(
                    "⬤ Stalled"
                )
                self.status_label.setStyleSheet(
                    "color: #ff6b6b; "
                    "font-weight: bold;"
                )
            self.last_seq_time = now

            self.statusBar().showMessage(
                f"Monitoring {self.shm_path} | "
                f"stale: {self.stale_count}"
            )

    def _update_rate(self):
        self.rate_label.setText(
            f"{self.updates_this_second} Hz"
        )
        self.updates_this_second = 0

# ------------------------------------
# Entry point
# ------------------------------------

def main():
    import argparse

    parser = argparse.ArgumentParser(
        description="NoLadder IO Monitor"
    )
    parser.add_argument(
        "config",
        nargs   = "?",
        default = "machine.toml",
        help    = "Path to machine.toml",
    )
    parser.add_argument(
        "--shm",
        default = "/dev/shm/noladder_io",
        help    = "Shared memory path",
    )
    args = parser.parse_args()

    app    = QApplication(sys.argv)
    window = MonitorWindow(
        shm_path    = args.shm,
        config_path = args.config,
    )

    # dark theme — easier on eyes in factory
    app.setStyleSheet("""
        QMainWindow, QWidget {
            background-color: #1e1e1e;
            color: #d4d4d4;
        }
        QGroupBox {
            border: 1px solid #444;
            border-radius: 4px;
            margin-top: 8px;
            padding-top: 8px;
            font-weight: bold;
        }
        QGroupBox::title {
            color: #9cdcfe;
            padding: 0 4px;
        }
        QTableWidget {
            background-color: #252526;
            alternate-background-color: #2d2d2d;
            border: none;
            gridline-color: #3e3e3e;
        }
        QHeaderView::section {
            background-color: #333;
            color: #9cdcfe;
            border: none;
            padding: 4px;
        }
        QLabel {
            padding: 2px 8px;
        }
    """)

    window.show()
    sys.exit(app.exec())

if __name__ == "__main__":
    main()
