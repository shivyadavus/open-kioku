def render_avatar(user_id: str) -> str:
    """Distractor profile rendering helper; no order persistence or video pipeline."""
    return f"avatar:{user_id}"
