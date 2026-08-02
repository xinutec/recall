package org.recall.mic

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt

/**
 * The segmented mic-level meter, shared by both modes' screens — streaming reads it from
 * the PCM it is about to send, the meeting recorder from `MediaRecorder`'s peak amplitude,
 * but the scaling ([amplitudeLevel]) and therefore the picture are the same. One
 * composable so "how loud does the room look" can't mean two different things.
 */
@Composable
fun LevelMeter(level: Float, segments: Int = 24) {
    val animated by animateFloatAsState(targetValue = level, label = "mic-level")
    val lit = (animated * segments).roundToInt()
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        for (i in 0 until segments) {
            Box(
                Modifier
                    .weight(1f)
                    .height(32.dp)
                    .clip(RoundedCornerShape(3.dp))
                    .background(segmentColor(meterTier(i, lit, segments))),
            )
        }
    }
}

@Composable
private fun segmentColor(tier: MeterTier) =
    when (tier) {
        MeterTier.OFF -> MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.22f)
        MeterTier.LOW -> MaterialTheme.colorScheme.primary
        MeterTier.MID -> MaterialTheme.colorScheme.tertiary
        MeterTier.HIGH -> MaterialTheme.colorScheme.error
    }
