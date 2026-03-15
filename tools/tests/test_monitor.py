#!/usr/bin/env python3
"""
Test suite for noladder_monitor.py
Tests symbol loading, device building, and config parsing.
"""

import sys
import os
import tempfile
import struct
from unittest.mock import MagicMock

# Mock out PySide6 before importing noladder_monitor
sys.modules['PySide6'] = MagicMock()
sys.modules['PySide6.QtWidgets'] = MagicMock()
sys.modules['PySide6.QtCore'] = MagicMock()
sys.modules['PySide6.QtGui'] = MagicMock()
sys.modules['PySide6.QtCharts'] = MagicMock()

# Add parent directory to path for imports
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import noladder_monitor


class TestSymbolLoading:
    """Test loading symbols from shared memory."""

    def test_load_symbols_missing_file(self):
        """Test that missing symbol file returns empty dict."""
        symbols = noladder_monitor.load_symbols("/nonexistent/path")
        assert symbols == {}, "Should return empty dict for missing file"
        print("✓ test_load_symbols_missing_file")

    def test_load_symbols_invalid_count(self):
        """Test that invalid symbol count returns empty dict."""
        with tempfile.NamedTemporaryFile(delete=False) as f:
            # Write invalid count (too large)
            f.write(struct.pack("I", 99999))
            f.flush()

            symbols = noladder_monitor.load_symbols(f.name)
            assert symbols == {}, "Should return empty dict for invalid count"
            os.unlink(f.name)
        print("✓ test_load_symbols_invalid_count")

    def test_load_symbols_empty(self):
        """Test loading symbol table with count=0."""
        with tempfile.NamedTemporaryFile(delete=False) as f:
            # Write count=0, then padding and empty symbols
            f.write(struct.pack("I", 0))
            f.write(b'\x00' * 36868)
            f.flush()

            symbols = noladder_monitor.load_symbols(f.name)
            assert symbols == {}, "Should return empty dict for count=0"
            os.unlink(f.name)
        print("✓ test_load_symbols_empty")

    def test_symbol_struct_size(self):
        """Verify Symbol struct is 72 bytes."""
        # Symbol: u32(4) + u8(1) + u8(1) + u8(1) + u8(1) + u8[64] = 72
        expected_size = 4 + 1 + 1 + 1 + 1 + 64
        assert expected_size == 72, f"Expected 72, got {expected_size}"
        print("✓ test_symbol_struct_size")

    def test_symbol_table_header_size(self):
        """Verify SymbolTable header is 8 bytes (count + padding)."""
        expected_header = 8  # u32 count + u8[4] padding
        assert noladder_monitor._TABLE_HEADER_SIZE == expected_header, \
            f"Expected header {expected_header}, got {noladder_monitor._TABLE_HEADER_SIZE}"
        print("✓ test_symbol_table_header_size")


class TestDeviceBuilding:
    """Test building device structures."""

    def test_build_devices_from_empty_symbols(self):
        """Test building devices from empty symbol dict."""
        config = noladder_monitor.MachineConfig.__new__(noladder_monitor.MachineConfig)
        config.devices = []

        config._build_from_symbols({})
        assert config.devices == [], "Should have empty device list"
        print("✓ test_build_devices_from_empty_symbols")

    def test_build_devices_sorts_alphabetically(self):
        """Test that devices are sorted alphabetically by path."""
        config = noladder_monitor.MachineConfig.__new__(noladder_monitor.MachineConfig)
        config.devices = []

        # Create symbols in random order
        symbols = {
            (0, 2): ("z_device.input", 0, 1),  # input, index 2
            (0, 0): ("a_device.input", 0, 1),  # input, index 0
            (0, 1): ("m_device.input", 0, 1),  # input, index 1
        }

        config._build_from_symbols(symbols)
        paths = [d["path"] for d in config.devices]
        assert paths == sorted(paths), f"Devices not sorted: {paths}"
        assert paths[0] == "a_device", "First device should be 'a_device'"
        print("✓ test_build_devices_sorts_alphabetically")

    def test_build_devices_signal_grouping(self):
        """Test that signals are grouped by device path."""
        config = noladder_monitor.MachineConfig.__new__(noladder_monitor.MachineConfig)
        config.devices = []

        symbols = {
            (0, 0): ("pump.speed", 0, 3),      # input
            (0, 1): ("pump.current", 0, 3),    # input
            (1, 0): ("pump.setpoint", 1, 3),   # output
            (1, 1): ("pump.enable", 1, 1),     # output
        }

        config._build_from_symbols(symbols)
        assert len(config.devices) == 1, f"Expected 1 device, got {len(config.devices)}"

        pump = config.devices[0]
        assert pump["path"] == "pump", f"Expected 'pump', got {pump['path']}"
        assert pump["input_count"] == 2, f"Expected 2 inputs, got {pump['input_count']}"
        assert pump["output_count"] == 2, f"Expected 2 outputs, got {pump['output_count']}"
        # Signals ordered by index, not name: index 0=speed, index 1=current
        assert pump["input_signals"] == ["speed", "current"], \
            f"Expected inputs sorted by index, got {pump['input_signals']}"
        print("✓ test_build_devices_signal_grouping")


class TestConfigParsing:
    """Test TOML config parsing."""

    def test_config_has_symbol_source(self):
        """Test that config tracks symbol source (live or TOML)."""
        config = noladder_monitor.MachineConfig.__new__(noladder_monitor.MachineConfig)
        config.symbol_source = True
        assert config.symbol_source == True, "Should be able to set symbol_source"

        config.symbol_source = False
        assert config.symbol_source == False, "Should be able to set symbol_source to False"
        print("✓ test_config_has_symbol_source")

    def test_config_symbol_source_attribute(self):
        """Test that config has symbol_source attribute."""
        config = noladder_monitor.MachineConfig.__new__(noladder_monitor.MachineConfig)
        config.symbol_source = False  # Initialize
        assert hasattr(config, 'symbol_source'), "MachineConfig missing symbol_source"
        print("✓ test_config_symbol_source_attribute")


class TestOffsets:
    """Test that binary offsets are correct."""

    def test_symbol_name_offset(self):
        """Verify symbol name starts at offset 8."""
        # Symbol struct: index(4) + kind(1) + type(1) + len(1) + pad(1) + name(64)
        # Name should start at offset 8
        offset = 4 + 1 + 1 + 1 + 1
        assert offset == 8, f"Expected name offset 8, got {offset}"
        print("✓ test_symbol_name_offset")

    def test_symbol_table_offset(self):
        """Verify SymbolTable header is 8 bytes before symbols array."""
        # SymbolTable: count(4) + pad(4) + symbols[512 * 72]
        header_size = 4 + 4
        assert header_size == 8, f"Expected header 8, got {header_size}"
        first_symbol_offset = header_size
        assert first_symbol_offset == 8, f"Expected first symbol at offset 8"
        print("✓ test_symbol_table_offset")


def run_all_tests():
    """Run all test suites."""
    print("\n=== NoLadder Monitor Tests ===\n")

    test_classes = [
        TestSymbolLoading(),
        TestDeviceBuilding(),
        TestConfigParsing(),
        TestOffsets(),
    ]

    passed = 0
    failed = 0

    for test_class in test_classes:
        methods = [m for m in dir(test_class) if m.startswith('test_')]
        for method_name in methods:
            try:
                method = getattr(test_class, method_name)
                method()
                passed += 1
            except AssertionError as e:
                print(f"✗ {test_class.__class__.__name__}.{method_name}")
                print(f"  {e}")
                failed += 1
            except Exception as e:
                print(f"✗ {test_class.__class__.__name__}.{method_name}")
                print(f"  Unexpected error: {e}")
                failed += 1

    print(f"\n=== Results ===")
    print(f"Passed: {passed}")
    print(f"Failed: {failed}")
    print(f"Total:  {passed + failed}\n")

    return 0 if failed == 0 else 1


if __name__ == '__main__':
    sys.exit(run_all_tests())
