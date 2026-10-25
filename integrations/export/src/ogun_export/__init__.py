"""ogun-export: sends run, partition and approval information out of Ogun.

The exporter follows the ``ogund`` event stream with an ``agent`` credential and forwards what it
sees to observability, lineage and chat systems. It only reads (``status``, ``list_runs``,
``get_run``, ``events_since``, ``list_approvals``, ``lineage``). It can never decide an approval,
and no secret value ever reaches a log, an error, an outbox file or a message.

Only the command line imports ``ogun-client``, and it does so lazily; everything else is the
standard library.
"""

import logging as _logging

__version__ = "0.1.0"

# A library does not print log records unless its caller configures logging.
_logging.getLogger("ogun_export").addHandler(_logging.NullHandler())
