#!/usr/bin/env python3
# HERDR_INTEGRATION_ID=tau
# HERDR_INTEGRATION_VERSION=1
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import herdr_wrapper
sys.exit(herdr_wrapper.main("tau", "TAU_ROOT", ["~/.herdr/tau", "~/tau", "../tau"]))
