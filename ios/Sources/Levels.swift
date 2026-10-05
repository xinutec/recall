import Foundation

/// The level meter's arithmetic, as Android's `Levels.kt`.
enum Levels {
    /// The quietest level shown, in dBFS.
    static let floorDbfs: Float = -70

    /// Peak amplitude of a block of signed 16-bit samples, normalised to 0...1.
    static func peakNormalised(_ samples: [Int16]) -> Float {
        var peak: Int32 = 0
        for s in samples {
            let a = Int32(s).magnitude
            if Int32(a) > peak { peak = Int32(a) }
        }
        return Float(peak) / 32768.0
    }

    /// Map a 0...1 peak to a 0...1 meter position on a dBFS scale (floor..0).
    static func meter(fromPeak peak: Float) -> Float {
        guard peak > 0 else { return 0 }
        let dbfs = 20 * log10(peak)
        let scaled = (dbfs - floorDbfs) / -floorDbfs
        return min(max(scaled, 0), 1)
    }

    /// The meter position for a sample block.
    static func meter(fromSamples samples: [Int16]) -> Float {
        meter(fromPeak: peakNormalised(samples))
    }

    /// A meter segment's colour, as Android's: green to 60%, orange to 85%, red
    /// above.
    enum MeterTier { case off, low, mid, high }

    static func tier(index: Int, lit: Int, segments: Int) -> MeterTier {
        if index >= lit { return .off }
        if Float(index) > Float(segments) * 0.85 { return .high }
        if Float(index) > Float(segments) * 0.6 { return .mid }
        return .low
    }
}
