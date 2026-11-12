from __future__ import annotations

import logging
import smtplib
import ssl
from dataclasses import dataclass
from email.message import EmailMessage
from typing import Protocol

from ..config import RuntimeConfig


_LOGGER = logging.getLogger(__name__)
SMTP_TIMEOUT_SECONDS = 10


class EmailDeliveryError(RuntimeError):
    """SMTP did not confirm acceptance of the message."""


@dataclass(frozen=True, slots=True)
class OutgoingEmail:
    recipient: str
    subject: str
    body: str


class EmailSender(Protocol):
    def send(self, message: OutgoingEmail) -> None: ...


class NullEmailSender:
    """Development-safe sender: delivery is deliberately disabled."""
    def send(self, message: OutgoingEmail) -> None:
        _LOGGER.warning('Email delivery is not configured; message was not sent.')


class SmtpEmailSender:
    def __init__(self, config: RuntimeConfig) -> None:
        self._host = config.smtp_host
        self._port = config.smtp_port
        self._username = config.smtp_username
        self._password = config.smtp_password
        self._from_email = config.smtp_from_email
        self._from_name = config.smtp_from_name or 'worta'
        self._security = config.smtp_security

    def send(self, message: OutgoingEmail) -> None:
        try:
            email = EmailMessage()
            email['From'] = f'{self._from_name} <{self._from_email}>'
            email['To'] = message.recipient
            email['Subject'] = message.subject
            email.set_content(message.body)
            context = ssl.create_default_context()
            if self._security == 'implicit_tls':
                with smtplib.SMTP_SSL(self._host, self._port, timeout=SMTP_TIMEOUT_SECONDS,
                                      context=context) as client:
                    client.login(self._username, self._password)
                    client.send_message(email)
            else:
                with smtplib.SMTP(self._host, self._port, timeout=SMTP_TIMEOUT_SECONDS) as client:
                    client.starttls(context=context)
                    client.login(self._username, self._password)
                    client.send_message(email)
        except (OSError, ValueError, smtplib.SMTPException):
            _LOGGER.warning('SMTP delivery failed.')
            raise EmailDeliveryError('SMTP delivery failed.') from None


class RecordingEmailSender:
    """Test boundary; never configured by production application code."""
    def __init__(self) -> None:
        self.messages: list[OutgoingEmail] = []

    def send(self, message: OutgoingEmail) -> None:
        self.messages.append(message)


def email_sender_from_config(config: RuntimeConfig) -> EmailSender:
    return SmtpEmailSender(config) if config.smtp_host else NullEmailSender()
