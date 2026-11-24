"""Public inbox budgets and the only message accepted before a claim."""
import re

# Setup shows the code as two groups of three: "6J8 E4K" and "6J8E4K" both count, in any case.
CLAIM = re.compile(r"([1-9A-HJKMNP-Z]{3})\s*([1-9A-HJKMNP-Z]{3})", re.IGNORECASE | re.ASCII)

# The owner's moderation commands skip every queue. Chatting with the model, owner or not, waits its turn.
MODERATION = frozenset({"/allow", "/revoke", "/anyone", "/private", "/status", "/model"})


def moderation(text):
    words = text.split()
    return bool(words) and words[0].lower().rstrip(".!?,;:") in MODERATION


PENDING_PER_SENDER = 2
SHARED_SLOTS = 8
OWNER_SLOTS = 2


class PublicPacing:
    """Each virtual deadline advances one refill interval per admitted request."""
    SENDER_BURST, SENDER_INTERVAL = 6, 5
    GLOBAL_BURST, GLOBAL_INTERVAL = 12, 3

    def __init__(self):
        self.senders = {}
        self.global_at = 0

    def available(self, sender, now):
        return (self.senders.get(sender, 0) <= now + (self.SENDER_BURST - 1) * self.SENDER_INTERVAL and
                self.global_at <= now + (self.GLOBAL_BURST - 1) * self.GLOBAL_INTERVAL)

    def spend(self, sender, now):
        self.senders = {peer: due for peer, due in self.senders.items() if due > now}
        self.senders[sender] = max(self.senders.get(sender, now), now) + self.SENDER_INTERVAL
        self.global_at = max(self.global_at, now) + self.GLOBAL_INTERVAL
