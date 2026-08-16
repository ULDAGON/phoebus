# Phoebus <- Omarchy bridge template.
#
# Install to ~/.config/omarchy/themed/phoebus.toml.tpl (contrib/omarchy/install.sh does
# it). omarchy-theme-set-templates renders it on every theme switch into
# ~/.local/state/omarchy/current/theme/phoebus.toml, which a running Phoebus notices
# within a second. The keys are Phoebus's palette tokens; the {{ placeholders }} are
# Omarchy's resolved theme colours — this file is where the two vocabularies meet.

mode = "{{ mode }}"
accent = "{{ accent }}"

bg0 = "{{ background }}"
bg1 = "{{ dark_background }}"
bg2 = "{{ lighter_background }}"
border = "{{ selection }}"

text_hi = "{{ foreground }}"
text_mid = "{{ mix foreground background 30% }}"
text_low = "{{ dark_foreground }}"
