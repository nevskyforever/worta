"""Release an active cloud project slot without removing its encrypted history."""
from alembic import op
import sqlalchemy as sa

revision = 'cloud_project_retirement'
down_revision = 'game_loss_v2'
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column('cloud_projects', sa.Column('retired_at', sa.DateTime(timezone=True), nullable=True))


def downgrade() -> None:
    db = op.get_bind()
    db.execute(sa.text('LOCK TABLE cloud_projects IN ACCESS EXCLUSIVE MODE'))
    if db.execute(sa.text('SELECT EXISTS (SELECT 1 FROM cloud_projects WHERE retired_at IS NOT NULL)')).scalar():
        raise RuntimeError('retired cloud projects exist')
    op.drop_column('cloud_projects', 'retired_at')
